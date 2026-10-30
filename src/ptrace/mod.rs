use std::collections::BTreeMap;
use std::convert::TryInto;
use std::ffi::CString;
use std::marker::PhantomData;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use procfs::process::{MMPermissions, Process};

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("toda process replacement requires Linux aarch64 or x86_64");
mod arch;

fn request(op: libc::c_uint, pid: i32, addr: usize, data: usize) -> Result<libc::c_long> {
    unsafe {
        *libc::__errno_location() = 0;
    }
    let value = unsafe {
        libc::ptrace(
            op,
            pid,
            addr as *mut libc::c_void,
            data as *mut libc::c_void,
        )
    };
    let error = std::io::Error::last_os_error();
    if value == -1 && error.raw_os_error() != Some(0) {
        return Err(error).with_context(|| format!("ptrace request {op:#x} for thread {pid}"));
    }
    Ok(value)
}

fn regset<T>(pid: i32, op: libc::c_uint, kind: usize, value: &mut T) -> Result<()> {
    let mut io = libc::iovec {
        iov_base: (value as *mut T).cast(),
        iov_len: std::mem::size_of::<T>(),
    };
    request(op, pid, kind, &mut io as *mut _ as usize)?;
    ensure!(
        io.iov_len == std::mem::size_of::<T>(),
        "unexpected register ABI for thread {pid}"
    );
    Ok(())
}

fn gone(pid: i32) -> bool {
    match Process::new(pid).and_then(|p| p.stat()) {
        Ok(stat) => matches!(stat.state, 'Z' | 'X' | 'x'),
        Err(procfs::ProcError::NotFound(_)) => true,
        Err(_) => false,
    }
}

fn wait_stop(pid: i32) -> Result<i32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, libc::__WALL | libc::WNOHANG) };
        if result == pid {
            ensure!(
                libc::WIFSTOPPED(status),
                "thread {pid} exited while traced (status {status:#x})"
            );
            return Ok(status);
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for stopped thread {pid}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[derive(Debug)]
struct Thread {
    stopped: bool,
    signal: i32,
}

#[derive(Debug)]
struct Patch {
    words: Vec<(u64, u64)>,
}

impl Patch {
    fn read(pid: i32, address: u64) -> Result<Self> {
        let mut patch = Self { words: Vec::new() };
        let end = address + arch::SYSCALL.len() as u64;
        for base in ((address & !7)..((end + 7) & !7)).step_by(8) {
            let original = request(libc::PTRACE_PEEKDATA, pid, base as usize, 0)? as u64;
            patch.words.push((base, original));
        }
        Ok(patch)
    }

    fn install(&self, pid: i32, address: u64) -> Result<()> {
        for &(base, original) in &self.words {
            let mut bytes = original.to_ne_bytes();
            for (i, byte) in arch::SYSCALL.iter().enumerate() {
                let at = address + i as u64;
                if (base..base + 8).contains(&at) {
                    bytes[(at - base) as usize] = *byte;
                }
            }
            request(
                libc::PTRACE_POKEDATA,
                pid,
                base as usize,
                u64::from_ne_bytes(bytes) as usize,
            )?;
        }
        Ok(())
    }

    fn restore(&self, pid: i32) -> Result<()> {
        for &(address, word) in &self.words {
            request(libc::PTRACE_POKEDATA, pid, address as usize, word as usize)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
enum Resume {
    Initial,
    Entry {
        registers: Box<arch::Registers>,
        number: u64,
    },
}

#[derive(Debug, PartialEq)]
enum Restoration {
    Pending,
    ExecutionRestored,
    Complete,
}

#[derive(Debug)]
struct Runner {
    initial: arch::Registers,
    mask: u64,
    resume: Resume,
    site: u64,
    patch: Option<Patch>,
    ready: bool,
    restoration: Restoration,
}

/// Owns every traced thread and any unfinished remote execution until explicit release.
#[derive(Debug)]
pub struct TracedProcess {
    pub pid: i32,
    threads: BTreeMap<i32, Thread>,
    runner: Option<Runner>,
    releasing: bool,
    scratch: Vec<(u64, u64)>,
    temporary_fds: Vec<u64>,
    _main_thread: PhantomData<Rc<()>>,
}

impl TracedProcess {
    pub(crate) fn new(pid: i32) -> Self {
        Self {
            pid,
            threads: BTreeMap::new(),
            runner: None,
            releasing: false,
            scratch: Vec::new(),
            temporary_fds: Vec::new(),
            _main_thread: PhantomData,
        }
    }

    pub(crate) fn acquire(&mut self) -> Result<()> {
        for _ in 0..32 {
            let tids: Vec<_> = Process::new(self.pid)?
                .tasks()?
                .map(|t| t.map(|t| t.tid))
                .collect::<std::result::Result<_, _>>()?;
            for tid in tids {
                if self.threads.contains_key(&tid) || gone(tid) {
                    continue;
                }
                let options = libc::PTRACE_O_TRACESYSGOOD | libc::PTRACE_O_TRACECLONE;
                if let Err(error) = request(libc::PTRACE_SEIZE, tid, 0, options as usize) {
                    if gone(tid) {
                        continue;
                    }
                    // A clone can be owned before its parent's clone event is consumed.
                    let owned = Process::new(tid)?.status()?.tracerpid == std::process::id() as i32;
                    if !owned {
                        return Err(error);
                    }
                }
                self.threads.insert(
                    tid,
                    Thread {
                        stopped: false,
                        signal: 0,
                    },
                );
                if let Err(error) = request(libc::PTRACE_INTERRUPT, tid, 0, 0) {
                    if !gone(tid)
                        && error
                            .downcast_ref::<std::io::Error>()
                            .and_then(|e| e.raw_os_error())
                            != Some(libc::EIO)
                    {
                        return Err(error);
                    }
                }
            }
            let pending: Vec<_> = self
                .threads
                .iter()
                .filter(|(_, t)| !t.stopped)
                .map(|(&tid, _)| tid)
                .collect();
            for tid in pending {
                if gone(tid) {
                    self.threads.remove(&tid);
                    continue;
                }
                let status = wait_stop(tid)?;
                self.threads.get_mut(&tid).unwrap().stopped = true;
                let event = status >> 16;
                if event == libc::PTRACE_EVENT_CLONE {
                    let mut child = 0u64;
                    request(
                        libc::PTRACE_GETEVENTMSG,
                        tid,
                        0,
                        &mut child as *mut _ as usize,
                    )?;
                    self.threads.entry(child as i32).or_insert(Thread {
                        stopped: false,
                        signal: 0,
                    });
                }
                ensure!(
                    event == 0
                        || event == libc::PTRACE_EVENT_STOP
                        || event == libc::PTRACE_EVENT_CLONE,
                    "unsupported ptrace event {event} for {tid}"
                );
                let thread = self.threads.get_mut(&tid).unwrap();
                thread.stopped = true;
                thread.signal = if event == 0 || libc::WSTOPSIG(status) != libc::SIGTRAP {
                    libc::WSTOPSIG(status)
                } else {
                    0
                };
            }
            if self.reconcile().is_ok() {
                ensure!(
                    self.threads.contains_key(&self.pid),
                    "process leader disappeared"
                );
                return Ok(());
            }
        }
        bail!("thread membership did not stabilize for {}", self.pid)
    }

    pub(crate) fn reconcile(&self) -> Result<()> {
        for task in Process::new(self.pid)?.tasks()? {
            let task = task?;
            if gone(task.tid) {
                continue;
            }
            let owned = self.threads.get(&task.tid).is_some_and(|t| t.stopped);
            let status = Process::new(task.tid)?.status()?;
            ensure!(
                owned
                    && status.tracerpid == std::process::id() as i32
                    && status.state.starts_with('t'),
                "live thread {} is not owned and stopped",
                task.tid
            );
        }
        Ok(())
    }

    pub(crate) fn prepare_runner(&mut self, excluded: &[(u64, u64)]) -> Result<()> {
        self.reconcile()?;
        let mut executable = std::fs::File::open(format!("/proc/{}/exe", self.pid))?;
        let mut elf = [0u8; 20];
        std::io::Read::read_exact(&mut executable, &mut elf)?;
        #[cfg(target_arch = "aarch64")]
        let machine = 183;
        #[cfg(target_arch = "x86_64")]
        let machine = 62;
        ensure!(
            &elf[..4] == b"\x7fELF"
                && elf[4] == 2
                && elf[5] == 1
                && u16::from_le_bytes([elf[18], elf[19]]) == machine,
            "only native 64-bit ELF targets are supported"
        );
        ensure!(
            self.threads.values().all(|t| t.signal == 0),
            "pending signal or group stop is unsupported during replacement"
        );
        let initial = arch::get(self.pid)?;
        let pc = arch::pc(&initial);
        let overlaps = |start: u64, end: u64| excluded.iter().any(|&(a, b)| start < b && a < end);
        let site = Process::new(self.pid)?
            .maps()?
            .into_iter()
            .find_map(|map| {
                let start = (map.address.0 + 7) & !7;
                (map.perms.contains(MMPermissions::EXECUTE)
                    && map.perms.contains(MMPermissions::PRIVATE)
                    && start + 8 <= map.address.1
                    && !overlaps(start, start + 8))
                .then_some(start)
            })
            .ok_or_else(|| anyhow!("no executable syscall bridge outside replacement mappings"))?;
        let mut instruction = vec![0u8; arch::SYSCALL.len()];
        self.read_memory(pc, &mut instruction)?;
        let at_syscall = instruction == arch::SYSCALL;
        let restarting = arch::restart_at_previous_instruction(&initial);
        let mut syscall_info = [0u8; 88];
        request(
            libc::PTRACE_GET_SYSCALL_INFO,
            self.pid,
            syscall_info.len(),
            syscall_info.as_mut_ptr() as usize,
        )?;
        let patch = Patch::read(self.pid, site)?;
        let mask = arch::block_signals(self.pid)?;
        self.runner = Some(Runner {
            initial,
            mask,
            resume: Resume::Initial,
            site,
            patch: None,
            ready: false,
            restoration: Restoration::Pending,
        });
        if at_syscall || restarting {
            let expected = if restarting {
                pc
            } else {
                pc + arch::SYSCALL.len() as u64
            };
            self.advance()?;
            let number = self.syscall_stop(1, expected)?;
            let registers = arch::get(self.pid)?;
            self.runner.as_mut().unwrap().resume = Resume::Entry {
                registers: Box::new(registers),
                number,
            };
            arch::substitute(self.pid, registers, libc::SYS_getpid as u64)?;
            self.advance()?;
            self.syscall_stop(2, expected)?;
            ensure!(
                arch::result(&arch::get(self.pid)?) == i64::from(self.pid),
                "bootstrap getpid returned wrong process"
            );
        }
        self.runner.as_mut().unwrap().ready = true;
        self.runner.as_mut().unwrap().patch = Some(patch);
        self.runner
            .as_ref()
            .unwrap()
            .patch
            .as_ref()
            .unwrap()
            .install(self.pid, site)?;
        Ok(())
    }

    fn advance(&mut self) -> Result<()> {
        request(libc::PTRACE_SYSCALL, self.pid, 0, 0)?;
        self.threads.get_mut(&self.pid).unwrap().stopped = false;
        let status = match wait_stop(self.pid) {
            Ok(status) => status,
            Err(error) => {
                let _ = request(libc::PTRACE_INTERRUPT, self.pid, 0, 0);
                if wait_stop(self.pid).is_ok() {
                    self.threads.get_mut(&self.pid).unwrap().stopped = true;
                }
                if let Some(runner) = &mut self.runner {
                    runner.ready = false;
                }
                return Err(error);
            }
        };
        self.threads.get_mut(&self.pid).unwrap().stopped = true;
        if libc::WSTOPSIG(status) != (libc::SIGTRAP | 0x80) || status >> 16 != 0 {
            if let Some(runner) = &mut self.runner {
                runner.ready = false;
            }
            bail!(
                "unexpected remote syscall stop {status:#x} for {}",
                self.pid
            );
        }
        Ok(())
    }

    fn syscall_stop(&mut self, op: u8, pc: u64) -> Result<u64> {
        let result = self.inspect_syscall_stop(op, pc);
        if result.is_err() {
            if let Some(runner) = &mut self.runner {
                runner.ready = false;
            }
        }
        result
    }

    fn inspect_syscall_stop(&self, op: u8, pc: u64) -> Result<u64> {
        let mut bytes = [0u8; 88];
        let len = request(
            libc::PTRACE_GET_SYSCALL_INFO,
            self.pid,
            bytes.len(),
            bytes.as_mut_ptr() as usize,
        )?;
        ensure!(
            len >= if op == 1 { 80 } else { 33 } && bytes[0] == op,
            "expected syscall stop {op}, got {} (length {len})",
            bytes[0]
        );
        ensure!(
            u32::from_ne_bytes(bytes[4..8].try_into().unwrap()) == arch::AUDIT_ARCH,
            "unexpected syscall ABI"
        );
        ensure!(
            u64::from_ne_bytes(bytes[8..16].try_into().unwrap()) == pc
                && arch::pc(&arch::get(self.pid)?) == pc,
            "unexpected syscall program counter"
        );
        Ok(u64::from_ne_bytes(bytes[24..32].try_into().unwrap()))
    }

    fn syscall(&mut self, number: libc::c_long, args: [u64; 6]) -> Result<u64> {
        let runner = self
            .runner
            .as_ref()
            .ok_or_else(|| anyhow!("syscall runner is unprepared"))?;
        ensure!(
            runner.ready && runner.restoration == Restoration::Pending && !self.releasing,
            "syscall runner requires recovery"
        );
        let mut regs = runner.initial;
        let site = runner.site;
        arch::set_pc(&mut regs, site);
        arch::call(&mut regs, number as u64, args);
        arch::set(self.pid, regs)?;
        self.runner.as_mut().unwrap().ready = false;
        arch::syscall_number(self.pid, -1)?;
        self.advance()?;
        ensure!(
            self.syscall_stop(1, site + arch::SYSCALL.len() as u64)? == number as u64,
            "unexpected remote syscall number"
        );
        self.advance()?;
        self.syscall_stop(2, site + arch::SYSCALL.len() as u64)?;
        self.runner.as_mut().unwrap().ready = true;
        let result = arch::result(&arch::get(self.pid)?);
        if (-4095..0).contains(&result) {
            return Err(std::io::Error::from_raw_os_error(-result as i32))
                .with_context(|| format!("remote syscall {number} in process {}", self.pid));
        }
        Ok(result as u64)
    }

    pub(crate) fn release(&mut self) -> Result<()> {
        if self.threads.keys().all(|&tid| gone(tid)) {
            self.threads.clear();
            self.runner = None;
            self.scratch.clear();
            self.temporary_fds.clear();
            return Ok(());
        }
        while let Some(&(address, length)) = self.scratch.last() {
            self.unmap(address, length)?;
            self.scratch.pop();
        }
        while let Some(&fd) = self.temporary_fds.last() {
            self.close(fd)?;
            self.temporary_fds.pop();
        }
        if let Some(runner) = &self.runner {
            if runner.restoration == Restoration::Pending {
                ensure!(
                    runner.ready,
                    "remote execution state remains uncertain for {}",
                    self.pid
                );
                if let Some(patch) = &runner.patch {
                    patch.restore(self.pid)?;
                }
                let initial = runner.initial;
                match &runner.resume {
                    Resume::Initial => {
                        arch::set(self.pid, initial)?;
                        arch::syscall_number(self.pid, -1)?;
                    }
                    Resume::Entry { registers, number } => {
                        let registers = **registers;
                        let number = *number;
                        let pc = arch::pc(&registers);
                        arch::set(self.pid, arch::replay(registers, &initial))?;
                        arch::syscall_number(self.pid, -1)?;
                        self.runner.as_mut().unwrap().ready = false;
                        self.advance()?;
                        ensure!(
                            self.syscall_stop(1, pc)? == number,
                            "restored syscall number differs"
                        );
                    }
                }
                self.runner.as_mut().unwrap().restoration = Restoration::ExecutionRestored;
            }
        }
        if let Some(runner) = &mut self.runner {
            if runner.restoration == Restoration::ExecutionRestored {
                arch::restore_signals(self.pid, runner.mask)?;
                runner.restoration = Restoration::Complete;
            }
        }
        self.releasing = true;
        let tids: Vec<_> = self.threads.keys().copied().collect();
        for tid in tids {
            if gone(tid) {
                self.threads.remove(&tid);
                continue;
            }
            if !self.threads[&tid].stopped {
                let _ = request(libc::PTRACE_INTERRUPT, tid, 0, 0);
                let status = wait_stop(tid)?;
                ensure!(
                    status >> 16 == libc::PTRACE_EVENT_STOP,
                    "unexpected stop while releasing thread {tid}"
                );
                self.threads.get_mut(&tid).unwrap().stopped = true;
            }
            let thread = &self.threads[&tid];
            request(libc::PTRACE_DETACH, tid, 0, thread.signal as usize)?;
            self.threads.remove(&tid);
        }
        Ok(())
    }

    pub(crate) fn is_gone(&self) -> bool {
        self.threads.keys().all(|&tid| gone(tid))
    }

    pub(crate) fn is_released(&self) -> bool {
        self.threads.is_empty()
    }

    pub fn read_memory(&self, address: u64, bytes: &mut [u8]) -> Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            let local = libc::iovec {
                iov_base: bytes[offset..].as_mut_ptr().cast(),
                iov_len: bytes.len() - offset,
            };
            let remote = libc::iovec {
                iov_base: (address + offset as u64) as *mut _,
                iov_len: local.iov_len,
            };
            let read = unsafe { libc::process_vm_readv(self.pid, &local, 1, &remote, 1, 0) };
            ensure!(
                read > 0,
                "read remote memory at {:#x}: {}",
                address + offset as u64,
                std::io::Error::last_os_error()
            );
            offset += read as usize;
        }
        Ok(())
    }

    pub fn write_mem(&self, address: u64, bytes: &[u8]) -> Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            let local = libc::iovec {
                iov_base: bytes[offset..].as_ptr() as *mut _,
                iov_len: bytes.len() - offset,
            };
            let remote = libc::iovec {
                iov_base: (address + offset as u64) as *mut _,
                iov_len: local.iov_len,
            };
            let written = unsafe { libc::process_vm_writev(self.pid, &local, 1, &remote, 1, 0) };
            ensure!(
                written > 0,
                "write remote memory at {:#x}: {}",
                address + offset as u64,
                std::io::Error::last_os_error()
            );
            offset += written as usize;
        }
        Ok(())
    }

    pub(crate) fn open(&mut self, path: &Path, flags: i32) -> Result<u64> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let bytes = path.as_bytes_with_nul();
        let address = self.map(
            bytes.len() as u64,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            u64::MAX,
            0,
        )?;
        self.scratch.push((address, bytes.len() as u64));
        self.write_mem(address, bytes)?;
        let result = self.syscall(
            libc::SYS_openat,
            [libc::AT_FDCWD as u64, address, flags as u64, 0, 0, 0],
        );
        if let Ok(fd) = result {
            self.temporary_fds.push(fd);
        }
        self.unmap(address, bytes.len() as u64)?;
        self.scratch.pop();
        if result.is_ok() {
            self.temporary_fds.pop();
        }
        result
    }

    pub(crate) fn close(&mut self, fd: u64) -> Result<()> {
        self.syscall(libc::SYS_close, [fd, 0, 0, 0, 0, 0])
            .map(|_| ())
    }
    pub(crate) fn fcntl(&mut self, fd: u64, op: i32, arg: u64) -> Result<u64> {
        self.syscall(libc::SYS_fcntl, [fd, op as u64, arg, 0, 0, 0])
    }
    pub(crate) fn seek(&mut self, fd: u64, offset: u64, whence: i32) -> Result<u64> {
        self.syscall(libc::SYS_lseek, [fd, offset, whence as u64, 0, 0, 0])
    }
    pub(crate) fn duplicate_to(&mut self, from: u64, to: u64, cloexec: bool) -> Result<()> {
        self.syscall(
            libc::SYS_dup3,
            [
                from,
                to,
                if cloexec { libc::O_CLOEXEC as u64 } else { 0 },
                0,
                0,
                0,
            ],
        )
        .map(|_| ())
    }
    pub(crate) fn fchdir(&mut self, fd: u64) -> Result<()> {
        self.syscall(libc::SYS_fchdir, [fd, 0, 0, 0, 0, 0])
            .map(|_| ())
    }
    pub(crate) fn map(
        &mut self,
        length: u64,
        prot: i32,
        flags: i32,
        fd: u64,
        offset: u64,
    ) -> Result<u64> {
        self.syscall(
            libc::SYS_mmap,
            [0, length, prot as u64, flags as u64, fd, offset],
        )
    }
    pub(crate) fn unmap(&mut self, address: u64, length: u64) -> Result<()> {
        self.syscall(libc::SYS_munmap, [address, length, 0, 0, 0, 0])
            .map(|_| ())
    }
    pub(crate) fn protect(&mut self, address: u64, length: u64, prot: i32) -> Result<()> {
        self.syscall(libc::SYS_mprotect, [address, length, prot as u64, 0, 0, 0])
            .map(|_| ())
    }
    pub(crate) fn remap(
        &mut self,
        address: u64,
        length: u64,
        flags: i32,
        destination: u64,
    ) -> Result<u64> {
        self.syscall(
            libc::SYS_mremap,
            [address, length, length, flags as u64, destination, 0],
        )
    }
}
