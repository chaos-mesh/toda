use crate::ptrace;

use std::path::{Path, PathBuf};
use std::fmt::Debug;
use std::fs::read_dir;
use std::fs::read_link;

use anyhow::{anyhow, Result};
use nix::fcntl::FcntlArg;
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;

use tracing::trace;

#[derive(PartialEq, Debug)]
enum MountDirection {
    EnableChaos,
    DisableChaos,
}

pub struct FdReplacer {
    pid: i32,
    original_path: PathBuf,
    new_path: PathBuf,
    direction: MountDirection,

    process: Option<ptrace::TracedProcess>,
}

impl FdReplacer {
    #[tracing::instrument()]
    pub fn new<P: AsRef<Path> + Debug>(path: P, pid: i32) -> Result<FdReplacer> {
        let original_path: PathBuf = path.as_ref().to_owned();

        let mut base_path: PathBuf = path.as_ref().to_owned();
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

        return Ok(FdReplacer {
            pid,
            original_path,
            new_path,
            direction: MountDirection::EnableChaos,
            process: None,
        });
    }

    #[tracing::instrument(skip(self))]
    pub fn trace(&mut self) -> Result<()> {
        self.process = Some(ptrace::TracedProcess::trace(self.pid)?);

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn reopen(&mut self) -> Result<()> {
        trace!("reopen fd for pid");

        let process = match self.process.as_mut() {
            Some(process) => process,
            None => {
                return Err(anyhow!("reopen is called before trace"))
            }
        };

        let base_path = if self.direction == MountDirection::EnableChaos {
            self.new_path.as_path()
        } else {
            self.original_path.as_path()
        };

        for thread in process.threads() {
            let tid = thread.tid;
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
                    if path.exists() && path.starts_with(base_path) {
                        self.reopen_file(&thread, fd, path.as_path())?;
                    }
                }
            }
        }

        if self.direction == MountDirection::EnableChaos {
            self.direction = MountDirection::DisableChaos
        } else {
            self.direction = MountDirection::EnableChaos
        }
        return Ok(());
    }

    #[tracing::instrument(skip(self))]
    pub fn detach(&mut self) -> Result<()> {
        let process = match self.process.take() {
            Some(process) => process,
            None => {
                return Err(anyhow!("reopen is called before trace"))
            }
        };

        for thread in process.threads() {
            thread.detach()?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self, thread, path))]
    fn reopen_file<P: AsRef<Path>>(&self, thread: &ptrace::TracedThread, fd: u64, path: P) -> Result<()> {
        trace!("reopen fd: {} for pid", fd);

        let base_path = if self.direction == MountDirection::EnableChaos {
            self.new_path.as_path()
        } else {
            self.original_path.as_path()
        };

        let striped_path = path.as_ref().strip_prefix(base_path)?;

        let original_path = if self.direction == MountDirection::EnableChaos {
            self.original_path.join(striped_path)
        } else {
            self.new_path.join(striped_path)
        };

        let flags = thread.fcntl(fd, FcntlArg::F_GETFL)?;

        let flags = OFlag::from_bits_truncate(flags as i32);

        let new_open_fd = thread.open(original_path, flags, Mode::empty())?;
        thread.dup2(new_open_fd, fd)?;
        thread.close(new_open_fd)?;

        return Ok(());
    }
}