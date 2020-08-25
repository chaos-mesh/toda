use crate::ptrace;

use std::fs::read_dir;
use std::fs::read_link;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use nix::fcntl::FcntlArg;
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;

use tracing::{info, warn};

pub struct FdReplacer {
    processes: Vec<ptrace::TracedProcess>,
}

pub fn encode_path<P: AsRef<Path>>(original_path: P) -> Result<(PathBuf, PathBuf)> {
    let original_path: PathBuf = original_path.as_ref().to_owned();

    let mut base_path: PathBuf = original_path.clone();
    if !base_path.pop() {
        return Err(anyhow!("path is the root"));
    }

    let mut new_path: PathBuf = base_path.clone();
    let original_filename = original_path
        .file_name()
        .ok_or(anyhow!("the path terminates in `..` or `/`"))?
        .to_str()
        .ok_or(anyhow!("path with non-UTF-8 character"))?;
    let new_filename = format!("__chaosfs__{}__", original_filename);
    new_path.push(new_filename.as_str());

    return Ok((original_path, new_path))
}

impl FdReplacer {
    #[tracing::instrument(skip(base_path))]
    pub fn prepare<P: AsRef<Path>>(
        base_path: P
    ) -> Result<FdReplacer> {
        let pids = read_dir("/proc")?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok());
        
        let mut processes = Vec::new();
        for pid in pids {
            let entries = match read_dir(format!("/proc/{}/fd", pid)) {
                Ok(entries)  => entries,
                Err(err) => {
                    warn!("fail to read /proc/{}/fd: {:?}", pid, err);
                    continue
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        warn!("fail to read entry {:?}", err);
                        continue;
                    }
                };

                let path = entry.path();
                if let Ok(path) = read_link(&path) {
                    if path.starts_with(base_path.as_ref()) {
                        processes.push(ptrace::TracedProcess::trace(pid)?)
                    }
                }
            }
        }

        return Ok(FdReplacer {
            processes,
        });
    }

    #[tracing::instrument(skip(self, original_path, new_path))]
    pub fn reopen<P1: AsRef<Path>, P2: AsRef<Path>>(&self, original_path: P1, new_path: P2) -> Result<()> {
        let base_path = original_path.as_ref();

        for process in self.processes.iter() {
            for thread in process.threads() {
                let tid = thread.tid;
                info!("reopen fd for tid: {}", tid);

                let fd_dir_path = format!("/proc/{}/fd", tid);
                for fd in read_dir(fd_dir_path)?.into_iter() {
                    let path = fd?.path();
                    let fd = path
                        .file_name()
                        .ok_or(anyhow!("fd doesn't contain a filename"))?
                        .to_str()
                        .ok_or(anyhow!("fd contains non-UTF-8 character"))?
                        .parse()?;
                    if let Ok(path) = read_link(&path) {
                        info!("handling path: {:?}", path);
                        if path.starts_with(base_path) {
                            info!("reopen file, fd: {:?}, path: {:?}", fd, path.as_path());
                            
                            let base_path = original_path.as_ref();
                            let striped_path = path.as_path().strip_prefix(base_path)?;
                            let new_path = new_path.as_ref().join(striped_path);
                            info!(
                                "reopen fd: {} for pid {}, from {} to {}",
                                fd,
                                thread.tid,
                                path.display(),
                                new_path.display()
                            );
                            self.reopen_file(&thread, fd, new_path.as_path())?;
                        }
                    }
                }
            }
        }

        return Ok(());
    }

    #[tracing::instrument(skip(self, thread, new_path))]
    fn reopen_file<P: AsRef<Path>>(
        &self,
        thread: &ptrace::TracedThread,
        fd: u64,
        new_path: P,
    ) -> Result<()> {
        let flags = thread.fcntl(fd, FcntlArg::F_GETFL)?;
        let flags = OFlag::from_bits_truncate(flags as i32);

        info!("fcntl get flags {:?}", flags);

        let new_open_fd = thread.open(new_path, flags, Mode::empty())?;
        thread.dup2(new_open_fd, fd)?;
        thread.close(new_open_fd)?;

        return Ok(());
    }
}

impl Drop for FdReplacer {
    #[tracing::instrument(skip(self))]
    fn drop(&mut self) {
        for process in self.processes.iter() {
            for thread in process.threads() {
                thread.detach().unwrap_or_else(|err| {
                    panic!(
                        "fails to detach thread {}: {}",
                        thread.tid, err
                    )
                });
            }
        }
    }
}
