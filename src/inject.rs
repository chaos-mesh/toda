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
use nix::sys::stat::Mode;
use nix::fcntl::OFlag;

#[derive(Default)]
pub struct InjectionBuilder {
    pid: Option<i32>,
    original_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
}

impl InjectionBuilder {
    pub fn new() -> InjectionBuilder {
        return InjectionBuilder::default();
    }

    pub fn path<P: AsRef<Path>>(self, path: P) -> Result<InjectionBuilder> {
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

        return Ok(InjectionBuilder {
            pid: self.pid,
            original_path: Some(original_path),
            new_path: Some(new_path),
        });
    }

    pub fn pid(self, pid: i32) -> Result<InjectionBuilder> {
        return Ok(InjectionBuilder {
            pid: Some(pid),
            original_path: self.original_path,
            new_path: self.new_path,
        });
    }

    pub fn mount(self) -> Result<Injection> {
        if let InjectionBuilder {
            pid: Some(pid),
            original_path: Some(original_path),
            new_path: Some(new_path),
        } = self
        {
            if mount::is_root(&original_path)? {
                // TODO: make the parent mount points private before move mount points
                mount::move_mount(&original_path, &new_path)?;
            } else {
                rename(&original_path, &new_path)?;
            }

            let fs = hookfs::HookFs::new(&original_path, &new_path);
            let session = unsafe {
                std::fs::create_dir_all(new_path.as_path())?;

                fuse::spawn_mount(fs, &original_path, &[])?
            };
            // TODO: remove this. But wait for FUSE gets up
            // Related Issue: https://github.com/zargony/fuse-rs/issues/9
            std::thread::sleep(std::time::Duration::from_secs(1));

            return Ok(Injection {
                pid,
                original_path,
                new_path,
                fuse_session: Some(session),
            });
        } else {
            return Err(anyhow!("run without setting path or pid"));
        }
    }
}

pub struct Injection {
    pid: i32,
    original_path: PathBuf,
    new_path: PathBuf,
    fuse_session: Option<BackgroundSession<'static>>,
}

impl Injection {
    pub fn reopen(&self) -> Result<()> {
        let process = ptrace::TracedProcess::trace(self.pid)?;

        for thread in process.threads() {
            let tid = thread.tid;
            let fd_dir_path = format!("/proc/{}/fd", tid);
            for fd in read_dir(fd_dir_path)?.into_iter() {
                let path = fd?.path();
                let fd = path
                    .file_name().ok_or(anyhow!("fd doesn't contain a filename"))?
                    .to_str().ok_or(anyhow!("fd contains non-UTF-8 character"))?
                    .parse()?;
                if let Ok(path) = read_link(&path) {
                    if path.exists() && path.starts_with(self.new_path.as_path()) {
                        self.reopen_file(&thread, fd, path.as_path())?;
                    }
                }
            }

            thread.cont()?;
        }
        return Ok(());
    }

    fn reopen_file<P: AsRef<Path>>(&self, thread: &TracedThread, fd: u64, path: P) -> Result<()> {
        let striped_path = path.as_ref().strip_prefix(self.new_path.as_path())?;
        let original_path = self.original_path.join(striped_path);

        let flags = thread.fcntl(fd, FcntlArg::F_GETFD)?;
        let mode = thread.fcntl(fd, FcntlArg::F_GETFL)? & 0003; // Only get Access Mode

        let flags = OFlag::from_bits(flags as i32).ok_or(anyhow!("flags is not available"))?;
        let mode = Mode::from_bits(mode as u32).ok_or(anyhow!("mode is not available"))?;
        
        // println!("Trying to open");
        let new_open_fd = thread.open(original_path, flags, mode)?;
        // let new_open_fd = thread.open(path, flags, mode)?;
        thread.dup2(new_open_fd, fd)?;
        
        return Ok(())
    }
}

impl Drop for Injection {
    fn drop(&mut self) {
        let injection = self.fuse_session.take().unwrap();
        drop(injection);

        if mount::is_root(&self.new_path).unwrap() {
            // TODO: make the parent mount points private before move mount points
            mount::move_mount(&self.new_path, &self.original_path).unwrap();
        } else {
            rename(&self.new_path, &self.original_path).unwrap();
        }
    }
}
