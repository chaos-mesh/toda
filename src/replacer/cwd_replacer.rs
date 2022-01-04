use super::ptrace;
use super::utils::all_processes;
use super::Replacer;

use std::fmt::Debug;
use std::path::{Path, PathBuf};

use anyhow::Result;

use tracing::{error, info, trace};

#[derive(Debug)]
pub struct CwdReplacer {
    processes: Vec<(ptrace::TracedProcess, PathBuf)>,
}

impl CwdReplacer {
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2,
    ) -> Result<CwdReplacer> {
        info!("preparing cmdreplacer");

        let processes = all_processes()?
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
            .filter_map(|(pid, path)| match ptrace::trace(pid) {
                Ok(process) => {
                    let mut new_path = new_path.as_ref().to_path_buf();

                    new_path.push(path.strip_prefix(detect_path.as_ref()).unwrap());
                    Some((process, new_path))
                }
                Err(err) => {
                    error!("fail to ptrace process: pid({}) with error: {:?}", pid, err);
                    None
                }
            })
            .collect();

        Ok(CwdReplacer { processes })
    }
}

impl Replacer for CwdReplacer {
    fn run(&mut self) -> Result<()> {
        info!("running cwd replacer");
        for (process, new_path) in self.processes.iter() {
            trace!("replacing cwd: {} to {:?}", process.pid, new_path);
            process.chdir(new_path)?;
        }

        Ok(())
    }
}
