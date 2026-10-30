use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use nix::sys::mman::{MapFlags, ProtFlags};
use procfs::process::{MMPermissions, MemoryMap};

use crate::ptrace::TracedProcess;

const DONTUNMAP: i32 = 4;

#[derive(Debug)]
enum MappingState {
    Planned,
    Staged(u64),
    OriginalMoved { stage: u64, backup: u64 },
    Installed { backup: u64 },
    Restored,
    Committed,
}

#[derive(Debug)]
pub(super) struct MappingChange {
    pub address: u64,
    pub length: u64,
    path: PathBuf,
    offset: u64,
    prot: i32,
    flags: i32,
    shared_writable: bool,
    state: MappingState,
    file: Option<u64>,
    probe: Option<u64>,
    probe_backup: Option<u64>,
}

impl MappingChange {
    pub fn new(map: MemoryMap, path: PathBuf) -> Self {
        let (prot, flags) = get_prot_and_flags_from_perms(map.perms);
        Self {
            address: map.address.0,
            length: map.address.1 - map.address.0,
            path,
            offset: map.offset,
            prot: prot as i32,
            flags: flags as i32,
            shared_writable: false,
            state: MappingState::Planned,
            file: None,
            probe: None,
            probe_backup: None,
        }
    }

    pub fn validate(&mut self, pid: i32, page: u64) -> Result<()> {
        ensure!(
            self.address.is_multiple_of(page)
                && self.length.is_multiple_of(page)
                && self.offset.is_multiple_of(page),
            "mapping is not page aligned"
        );
        ensure!(
            self.prot & libc::PROT_READ != 0,
            "non-readable file mapping at {:#x} is unsupported",
            self.address
        );
        let smaps = std::fs::read_to_string(format!("/proc/{pid}/smaps"))?;
        let prefix = format!("{:08x}-{:08x} ", self.address, self.address + self.length);
        let section = smaps
            .split(&prefix)
            .nth(1)
            .context("mapping disappeared from smaps")?;
        let flags = section
            .lines()
            .find_map(|line| line.strip_prefix("VmFlags:"))
            .context("missing mapping flags")?;
        ensure!(
            flags.split_whitespace().all(|flag| matches!(
                flag,
                "rd" | "wr" | "ex" | "sh" | "mr" | "mw" | "me" | "ms" | "ac" | "sd"
            )),
            "unsupported mapping flags at {:#x}: {flags}",
            self.address
        );
        self.shared_writable =
            self.flags == libc::MAP_SHARED && flags.split_whitespace().any(|flag| flag == "mw");
        File::open(format!("/proc/{pid}/pagemap"))?;
        Ok(())
    }

    pub fn stage(&mut self, process: &mut TracedProcess, page: u64) -> Result<()> {
        if !matches!(self.state, MappingState::Planned) {
            return Ok(());
        }
        let flags = if self.shared_writable {
            libc::O_RDWR
        } else {
            libc::O_RDONLY
        };
        let fd = process.open(&self.path, flags | libc::O_CLOEXEC)?;
        self.file = Some(fd);
        let probe = process.map(page, libc::PROT_READ, self.flags, fd, self.offset)?;
        self.probe = Some(probe);
        let backup = process.remap(probe, page, libc::MREMAP_MAYMOVE | DONTUNMAP, 0)?;
        self.probe_backup = Some(backup);
        ensure!(
            backup != probe,
            "DONTUNMAP did not retain a separate mapping"
        );
        let restored = process.remap(
            backup,
            page,
            libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
            probe,
        )?;
        ensure!(
            restored == probe,
            "mapping capability probe restored the wrong address"
        );
        self.probe_backup = None;
        process.unmap(probe, page)?;
        self.probe = None;
        let prot = if self.flags == libc::MAP_PRIVATE {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            self.prot
        };
        let stage = process.map(self.length, prot, self.flags, fd, self.offset)?;
        self.state = MappingState::Staged(stage);
        if self.flags == libc::MAP_PRIVATE {
            let pagemap = File::open(format!("/proc/{}/pagemap", process.pid))?;
            let mut bytes = vec![0u8; page as usize];
            for offset in (0..self.length).step_by(page as usize) {
                process.read_memory(self.address + offset, &mut bytes)?;
                let mut entry = [0u8; 8];
                pagemap.read_exact_at(&mut entry, (self.address + offset) / page * 8)?;
                let bits = u64::from_ne_bytes(entry);
                ensure!(
                    bits & (1 << 63) != 0 && bits & (1 << 62) == 0,
                    "private page is not resident or is swapped"
                );
                if bits & (1 << 61) == 0 {
                    process.write_mem(stage + offset, &bytes)?;
                }
            }
        }
        process.protect(stage, self.length, self.prot)?;
        Ok(())
    }

    pub fn install(&mut self, process: &mut TracedProcess) -> Result<()> {
        if let MappingState::Staged(stage) = self.state {
            let backup = process.remap(
                self.address,
                self.length,
                libc::MREMAP_MAYMOVE | DONTUNMAP,
                0,
            )?;
            self.state = MappingState::OriginalMoved { stage, backup };
            ensure!(
                backup != self.address && backup != stage,
                "original mapping was not retained separately"
            );
        }
        if let MappingState::OriginalMoved { stage, backup } = self.state {
            let address = process.remap(
                stage,
                self.length,
                libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
                self.address,
            )?;
            ensure!(
                address == self.address,
                "replacement mapping returned the wrong address"
            );
            self.state = MappingState::Installed { backup };
        }
        Ok(())
    }

    pub fn rollback(&mut self, process: &mut TracedProcess) -> Result<()> {
        match self.state {
            MappingState::OriginalMoved { stage, backup } => {
                let address = process.remap(
                    backup,
                    self.length,
                    libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
                    self.address,
                )?;
                ensure!(
                    address == self.address,
                    "rollback returned the wrong address"
                );
                self.state = MappingState::Staged(stage);
            }
            MappingState::Installed { backup } => {
                let address = process.remap(
                    backup,
                    self.length,
                    libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
                    self.address,
                )?;
                ensure!(
                    address == self.address,
                    "rollback returned the wrong address"
                );
                self.state = MappingState::Restored;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn cleanup(&mut self, process: &mut TracedProcess, page: u64) -> Result<()> {
        match self.state {
            MappingState::Installed { backup } => {
                process.unmap(backup, self.length)?;
                self.state = MappingState::Committed;
            }
            MappingState::Staged(stage) => {
                process.unmap(stage, self.length)?;
                self.state = MappingState::Restored;
            }
            MappingState::OriginalMoved { .. } => {
                anyhow::bail!("original mapping still requires rollback")
            }
            _ => {}
        }
        if let Some(backup) = self.probe_backup {
            process.unmap(backup, page)?;
            self.probe_backup = None;
        }
        if let Some(probe) = self.probe {
            process.unmap(probe, page)?;
            self.probe = None;
        }
        if let Some(fd) = self.file {
            process.close(fd)?;
            self.file = None;
        }
        Ok(())
    }
}

fn get_prot_and_flags_from_perms(perms: MMPermissions) -> (u64, u64) {
    let mut prot = ProtFlags::empty();
    if perms.contains(MMPermissions::READ) {
        prot |= ProtFlags::PROT_READ;
    }
    if perms.contains(MMPermissions::WRITE) {
        prot |= ProtFlags::PROT_WRITE;
    }
    if perms.contains(MMPermissions::EXECUTE) {
        prot |= ProtFlags::PROT_EXEC;
    }
    let flags = if perms.contains(MMPermissions::SHARED) {
        MapFlags::MAP_SHARED
    } else {
        MapFlags::MAP_PRIVATE
    };
    (prot.bits() as u64, flags.bits() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_permissions_preserve_protection_and_sharing() {
        let cases = [
            (
                MMPermissions::NONE,
                ProtFlags::empty(),
                MapFlags::MAP_PRIVATE,
            ),
            (
                MMPermissions::READ | MMPermissions::PRIVATE,
                ProtFlags::PROT_READ,
                MapFlags::MAP_PRIVATE,
            ),
            (
                MMPermissions::WRITE | MMPermissions::SHARED,
                ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
            ),
            (
                MMPermissions::EXECUTE | MMPermissions::PRIVATE,
                ProtFlags::PROT_EXEC,
                MapFlags::MAP_PRIVATE,
            ),
            (
                MMPermissions::READ
                    | MMPermissions::WRITE
                    | MMPermissions::EXECUTE
                    | MMPermissions::SHARED,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC,
                MapFlags::MAP_SHARED,
            ),
        ];
        for (perms, prot, flags) in cases {
            assert_eq!(
                get_prot_and_flags_from_perms(perms),
                (prot.bits() as u64, flags.bits() as u64)
            );
        }
    }
}
