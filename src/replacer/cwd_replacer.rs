use crate::ptrace;

use super::Replacer;

use std::fmt::Debug;
use std::path::{Path, PathBuf};

use anyhow::Result;

use log::{error, info, trace};

use procfs::process::all_processes;

#[derive(Debug)]
pub struct CwdReplacer<'a> {
    processes: Vec<ptrace::TracedProcess<'a>>,
    new_path: PathBuf,
}

impl<'a> CwdReplacer<'a> {
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2,
        ptrace_manager: &'a ptrace::PtraceManager,
    ) -> Result<CwdReplacer<'a>> {
        info!("preparing cmdreplacer");

        let processes = all_processes()?
            .into_iter()
            .filter(|process| -> bool {
                if let Ok(stat) = process.stat() {
                    !stat.comm.contains("toda")
                } else {
                    true
                }
            })
            .filter_map(|process| -> Option<_> {
                let pid = process.pid;
                trace!("itering proc: {}", pid);

                match process.cwd() {
                    Ok(cwd) => Some((pid, cwd)),
                    Err(err) => {
                        trace!("filter out pid({}) because of error: {:?}", pid, err);
                        None
                    }
                }
            })
            .filter(|(_, path)| path.starts_with(detect_path.as_ref()))
            .filter_map(|(pid, _)| match ptrace_manager.trace(pid) {
                Ok(process) => Some(process),
                Err(err) => {
                    error!("fail to ptrace process: pid({}) with error: {:?}", pid, err);
                    None
                }
            })
            .collect();

        Ok(CwdReplacer {
            processes,
            new_path: new_path.as_ref().to_owned(),
        })
    }
}

impl<'a> Replacer for CwdReplacer<'a> {
    fn run(&mut self) -> Result<()> {
        info!("running cwd replacer");
        for process in self.processes.iter() {
            process.chdir(&self.new_path)?;
        }

        Ok(())
    }
}
