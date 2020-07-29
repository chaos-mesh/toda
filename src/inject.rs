use crate::hookfs;
use crate::mount;
use crate::ptrace;
use crate::ptrace::TracedThread;

use std::fs::read_dir;
use std::fs::read_link;
use std::fs::rename;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use fuse::BackgroundSession;
use nix::fcntl::FcntlArg;
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;

use tracing::trace;

#[derive(PartialEq, Debug)]
enum MountDirection {
    EnableChaos,
    DisableChaos,
}

#[derive(Debug)]
pub struct Injection {
    pid: i32,
    original_path: PathBuf,
    new_path: PathBuf,
    fuse_session: Option<BackgroundSession<'static>>,
    direction: MountDirection,
    mounts: mount::MountsInfo,
}

impl Injection {
    pub fn create_injection<P: AsRef<Path>>(path: P, pid: i32) -> Result<Injection> {
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

        return Ok(Injection {
            pid,
            original_path,
            new_path,
            fuse_session: None,
            direction: MountDirection::EnableChaos,
            mounts: mount::MountsInfo::parse_mounts()?,
        });
    }

    pub fn mount(&mut self) -> Result<()> {
        if self.mounts.is_root(&self.original_path)? {
            // TODO: make the parent mount points private before move mount points
            self.mounts.move_mount(&self.original_path, &self.new_path)?;
        } else {
            rename(&self.original_path, &self.new_path)?;
        }

        let fs = hookfs::HookFs::new(&self.original_path, &self.new_path);
        let session = unsafe {
            std::fs::create_dir_all(self.new_path.as_path())?;

            fuse::spawn_mount(fs, &self.original_path, &[])?
        };
        // TODO: remove this. But wait for FUSE gets up
        // Related Issue: https://github.com/zargony/fuse-rs/issues/9
        std::thread::sleep(std::time::Duration::from_secs(1));

        self.fuse_session = Some(session);

        return Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn reopen(&mut self) -> Result<()> {
        trace!("reopen fd for pid");

        let process = ptrace::TracedProcess::trace(self.pid)?;

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

            thread.detach()?;
        }

        if self.direction == MountDirection::EnableChaos {
            self.direction = MountDirection::DisableChaos
        } else {
            self.direction = MountDirection::EnableChaos
        }
        return Ok(());
    }

    #[tracing::instrument(skip(self, thread, path))]
    fn reopen_file<P: AsRef<Path>>(&self, thread: &TracedThread, fd: u64, path: P) -> Result<()> {
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

    #[tracing::instrument(skip(self))]
    pub fn recover_mount(&mut self) -> Result<()> {
        let injection = self.fuse_session.take().unwrap();
        drop(injection);

        // TODO: replace the fd back and force remove the mount
        if self.mounts.is_root(&self.original_path)? {
            // TODO: make the parent mount points private before move mount points
            self.mounts.move_mount(&self.new_path, &self.original_path)?;
        } else {
            rename(&self.new_path, &self.original_path)?;
        }

        return Ok(());
    }
}
