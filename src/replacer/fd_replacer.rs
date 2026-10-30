use std::path::PathBuf;

use anyhow::{ensure, Context, Result};

use crate::ptrace::TracedProcess;

#[derive(Debug)]
pub(super) struct FdChange {
    pub target: u64,
    pub path: PathBuf,
    flags: i32,
    cloexec: bool,
    position: Option<u64>,
    original: Option<u64>,
    replacement: Option<u64>,
    installed: bool,
}

impl FdChange {
    pub fn new(target: u64, path: PathBuf) -> Self {
        Self {
            target,
            path,
            flags: 0,
            cloexec: false,
            position: None,
            original: None,
            replacement: None,
            installed: false,
        }
    }

    pub fn prepare(&mut self, process: &mut TracedProcess) -> Result<()> {
        self.flags = process.fcntl(self.target, libc::F_GETFL, 0)? as i32;
        let metadata = std::fs::metadata(format!("/proc/{}/fd/{}", process.pid, self.target))?;
        ensure!(
            metadata.is_file() || metadata.is_dir() || self.flags & libc::O_PATH != 0,
            "only regular, directory, and O_PATH descriptors can be replaced"
        );
        self.cloexec = process.fcntl(self.target, libc::F_GETFD, 0)? & libc::FD_CLOEXEC as u64 != 0;
        self.original = Some(process.fcntl(self.target, libc::F_DUPFD_CLOEXEC, 0)?);
        if self.flags & libc::O_PATH == 0 {
            match process.seek(self.target, 0, libc::SEEK_CUR) {
                Ok(position) => self.position = Some(position),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .and_then(|e| e.raw_os_error())
                        == Some(libc::ESPIPE) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn stage(&mut self, process: &mut TracedProcess) -> Result<()> {
        if self.replacement.is_some() {
            return Ok(());
        }
        let flags =
            (self.flags & !(libc::O_CREAT | libc::O_EXCL | libc::O_TRUNC)) | libc::O_CLOEXEC;
        let fd = process
            .open(&self.path, flags)
            .with_context(|| format!("reopen fd {} at {}", self.target, self.path.display()))?;
        self.replacement = Some(fd);
        if self.flags & libc::O_PATH == 0 {
            process.fcntl(fd, libc::F_SETFL, self.flags as u64)?;
        }
        if let Some(position) = self.position {
            process.seek(fd, position, libc::SEEK_SET)?;
        }
        Ok(())
    }

    pub fn install(&mut self, process: &mut TracedProcess) -> Result<()> {
        if !self.installed {
            process.duplicate_to(
                self.replacement.context("fd was not staged")?,
                self.target,
                self.cloexec,
            )?;
            self.installed = true;
        }
        Ok(())
    }

    pub fn rollback(&mut self, process: &mut TracedProcess) -> Result<()> {
        if self.installed {
            process.duplicate_to(
                self.original.context("original fd is unavailable")?,
                self.target,
                self.cloexec,
            )?;
            self.installed = false;
        }
        Ok(())
    }

    pub fn cleanup(&mut self, process: &mut TracedProcess) -> Result<()> {
        if let Some(fd) = self.replacement {
            process.close(fd)?;
            self.replacement = None;
        }
        if let Some(fd) = self.original {
            process.close(fd)?;
            self.original = None;
        }
        Ok(())
    }
}
