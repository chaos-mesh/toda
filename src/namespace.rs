use anyhow::Result;

use nix::fcntl::{open, OFlag};
use nix::sched::setns;
use nix::sched::CloneFlags;
use nix::sys::stat;

use crate::thread;

pub fn enter_mnt_namespace(pid: i32) -> Result<()> {
    let mnt_ns_path = format!("/proc/{}/ns/mnt", pid);
    let mnt_ns = open(mnt_ns_path.as_str(), OFlag::O_RDONLY, stat::Mode::all())?;

    setns(mnt_ns, CloneFlags::CLONE_NEWNS)?;

    Ok(())
}

pub fn with_mnt_pid_namespace<F, R>(
    f: Box<F>,
    pid: i32,
) -> Result<thread::JoinHandle<Result<R>>> 
where 
    F: FnOnce() -> Result<R>, 
    F: Send + 'static,
    R: Send + 'static {
    let pid_ns_path = format!("/proc/{}/ns/pid", pid);
    let pid_ns = open(pid_ns_path.as_str(), OFlag::O_RDONLY, stat::Mode::all())?;

    setns(pid_ns, CloneFlags::CLONE_NEWPID)?;

    Ok(thread::spawn(move || -> Result<R> {
        if let Err(err) = enter_mnt_namespace(pid) {
            Err(err)
        } else {
            f()
        }
    }))
}
