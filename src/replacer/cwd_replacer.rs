use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::ptrace::TracedProcess;

#[derive(Debug)]
pub(super) struct CwdChange {
    pub path: PathBuf,
    original: Option<u64>,
    replacement: Option<u64>,
    installed: bool,
}

impl CwdChange {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            original: None,
            replacement: None,
            installed: false,
        }
    }
    pub fn prepare(&mut self, process: &mut TracedProcess) -> Result<()> {
        self.original = Some(process.open(
            Path::new("."),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )?);
        Ok(())
    }
    pub fn stage(&mut self, process: &mut TracedProcess) -> Result<()> {
        if self.replacement.is_none() {
            self.replacement = Some(process.open(
                &self.path,
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )?);
        }
        Ok(())
    }
    pub fn install(&mut self, process: &mut TracedProcess) -> Result<()> {
        if !self.installed {
            process.fchdir(self.replacement.context("cwd was not staged")?)?;
            self.installed = true;
        }
        Ok(())
    }
    pub fn rollback(&mut self, process: &mut TracedProcess) -> Result<()> {
        if self.installed {
            process.fchdir(self.original.context("original cwd is unavailable")?)?;
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
