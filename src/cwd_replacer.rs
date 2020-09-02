use crate::ptrace;
use crate::utils;

use std::path::{Path, PathBuf};
use std::{collections::HashMap, fmt::Debug};
use std::fs::read_link;

use anyhow::{anyhow, Result};

#[derive(Debug)]
pub struct CwdReplacer {
    processes: Vec<ptrace::TracedProcess>,
    new_path: PathBuf,
}

impl CwdReplacer {
    #[tracing::instrument(skip(detect_path, new_path))]
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2,
    ) -> Result<CwdReplacer> {
        let pids = utils::iter_pids()?;
        let mut processes = Vec::new();

        for pid in pids {
            let cwd = PathBuf::from(format!("/proc/{}/cwd", pid));
            if let Ok(path) = read_link(&cwd) {
                if path.starts_with(detect_path.as_ref()) {
                    processes.push(ptrace::TracedProcess::trace(pid)?)
                }
            }
        }

        Ok(CwdReplacer {
            processes,
            new_path: new_path.as_ref().to_owned()
        })
    }

    #[tracing::instrument(skip(self))]
    pub fn run(&mut self) -> Result<()> {
        for process in self.processes.iter() {
            process.chdir(&self.new_path)?;
        }

        Ok(())
    }
}

impl Drop for CwdReplacer {
    #[tracing::instrument(skip(self))]
    fn drop(&mut self) {
        for process in self.processes.iter() {
            process.detach()
                .unwrap_or_else(|err| panic!("fails to detach process {}: {}", process.pid, err))
        }
    }
}
