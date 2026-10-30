use std::path::Path;

use anyhow::{ensure, Context, Result};
use procfs::process::{FDTarget, MMapPath, Process};

use crate::ptrace::TracedProcess;
use cwd_replacer::CwdChange;
use fd_replacer::FdChange;
use mmap_replacer::MappingChange;

mod cwd_replacer;
mod fd_replacer;
mod mmap_replacer;
mod utils;

pub trait Replacer {
    fn run(&mut self) -> Result<()>;
}

#[derive(Debug)]
struct ProcessReplacement {
    process: TracedProcess,
    fds: Vec<FdChange>,
    cwd: Option<CwdChange>,
    mappings: Vec<MappingChange>,
    prepared: bool,
}

#[derive(Debug, Default, PartialEq)]
enum Disposition {
    #[default]
    Pending,
    Commit,
    Rollback,
    Released,
}

#[derive(Debug, Default)]
pub struct UnionReplacer {
    processes: Vec<ProcessReplacement>,
    disposition: Disposition,
}

fn candidate(process: &Process, path: &Path) -> bool {
    process.cwd().is_ok_and(|cwd| cwd.starts_with(path))
        || process.fd().is_ok_and(|fds| {
            fds.filter_map(Result::ok)
                .any(|entry| matches!(entry.target, FDTarget::Path(p) if p.starts_with(path)))
        })
        || process.maps().is_ok_and(|maps| {
            maps.iter()
                .any(|map| matches!(&map.pathname, MMapPath::Path(p) if p.starts_with(path)))
        })
}

fn page_size() -> u64 {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 }
}

impl UnionReplacer {
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        &mut self,
        detect_path: P1,
        new_path: P2,
    ) -> Result<()> {
        ensure!(
            self.processes.is_empty() && self.disposition == Disposition::Pending,
            "replacement batch already prepared"
        );
        let detect = detect_path.as_ref();
        let replacement = new_path.as_ref();
        for process in utils::all_processes()? {
            if !candidate(&process, detect) {
                continue;
            }
            self.processes.push(ProcessReplacement {
                process: TracedProcess::new(process.pid),
                fds: Vec::new(),
                cwd: None,
                mappings: Vec::new(),
                prepared: false,
            });
            let plan = self.processes.last_mut().unwrap();
            plan.process
                .acquire()
                .with_context(|| format!("stop candidate process {}", process.pid))?;
            for entry in process.fd()? {
                let entry = entry?;
                if let FDTarget::Path(path) = entry.target {
                    if let Ok(relative) = path.strip_prefix(detect) {
                        ensure!(
                            !path.as_os_str().to_string_lossy().ends_with(" (deleted)"),
                            "deleted open files cannot be reopened"
                        );
                        plan.fds
                            .push(FdChange::new(entry.fd as u64, replacement.join(relative)));
                    }
                }
            }
            let cwd = process.cwd()?;
            if let Ok(relative) = cwd.strip_prefix(detect) {
                plan.cwd = Some(CwdChange::new(replacement.join(relative)));
            }
            for map in process.maps()? {
                if let MMapPath::Path(path) = &map.pathname {
                    if let Ok(relative) = path.strip_prefix(detect) {
                        let target = replacement.join(relative);
                        ensure!(
                            !path.as_os_str().to_string_lossy().ends_with(" (deleted)"),
                            "deleted mappings cannot be reopened"
                        );
                        ensure!(
                            std::fs::metadata(
                                std::path::PathBuf::from(format!("/proc/{}/root", process.pid))
                                    .join(path.strip_prefix("/")?)
                            )?
                            .is_file(),
                            "only regular file mappings are supported"
                        );
                        let mut change = MappingChange::new(map, target);
                        change.validate(process.pid, page_size())?;
                        plan.mappings.push(change);
                    }
                }
            }
            let excluded: Vec<_> = plan
                .mappings
                .iter()
                .map(|m| (m.address, m.address + m.length))
                .collect();
            plan.process.prepare_runner(&excluded)?;
            plan.prepared = true;
            for fd in &mut plan.fds {
                fd.prepare(&mut plan.process)?;
            }
            if let Some(cwd) = &mut plan.cwd {
                cwd.prepare(&mut plan.process)?;
            }
        }
        Ok(())
    }

    pub fn rollback(&mut self) -> Result<()> {
        ensure!(
            self.disposition != Disposition::Commit && self.disposition != Disposition::Released,
            "committed replacement cannot roll back"
        );
        self.disposition = Disposition::Rollback;
        for plan in self.processes.iter_mut().rev() {
            if plan.process.is_gone() {
                continue;
            }
            for map in plan.mappings.iter_mut().rev() {
                map.rollback(&mut plan.process)?;
            }
            if let Some(cwd) = &mut plan.cwd {
                cwd.rollback(&mut plan.process)?;
            }
            for fd in plan.fds.iter_mut().rev() {
                fd.rollback(&mut plan.process)?;
            }
        }
        self.cleanup()
    }

    pub fn commit(&mut self) -> Result<()> {
        ensure!(
            self.disposition != Disposition::Rollback,
            "rolled-back replacement cannot commit"
        );
        self.disposition = Disposition::Commit;
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<()> {
        for plan in &mut self.processes {
            if plan.prepared && !plan.process.is_released() && !plan.process.is_gone() {
                for map in &mut plan.mappings {
                    map.cleanup(&mut plan.process, page_size())?;
                }
                if let Some(cwd) = &mut plan.cwd {
                    cwd.cleanup(&mut plan.process)?;
                }
                for fd in &mut plan.fds {
                    fd.cleanup(&mut plan.process)?;
                }
            }
        }
        Ok(())
    }

    pub fn release(&mut self) -> Result<()> {
        ensure!(
            self.disposition != Disposition::Pending,
            "select commit or rollback before releasing processes"
        );
        if self.disposition == Disposition::Released {
            return Ok(());
        }
        self.cleanup()?;
        for plan in &mut self.processes {
            plan.process.release()?;
        }
        self.disposition = Disposition::Released;
        Ok(())
    }
}

impl Replacer for UnionReplacer {
    fn run(&mut self) -> Result<()> {
        ensure!(
            self.disposition == Disposition::Pending,
            "replacement batch is already finishing"
        );
        for plan in &mut self.processes {
            plan.process.reconcile()?;
            for fd in &mut plan.fds {
                fd.stage(&mut plan.process)?;
            }
            if let Some(cwd) = &mut plan.cwd {
                cwd.stage(&mut plan.process)?;
            }
            for map in &mut plan.mappings {
                map.stage(&mut plan.process, page_size())?;
            }
        }
        for plan in &mut self.processes {
            plan.process.reconcile()?;
        }
        for plan in &mut self.processes {
            for fd in &mut plan.fds {
                fd.install(&mut plan.process)?;
            }
            if let Some(cwd) = &mut plan.cwd {
                cwd.install(&mut plan.process)?;
            }
            for map in &mut plan.mappings {
                map.install(&mut plan.process)?;
            }
        }
        Ok(())
    }
}
