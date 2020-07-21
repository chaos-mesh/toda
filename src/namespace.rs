use anyhow::Result;
use nix::fcntl::{open, OFlag};
use nix::sched::setns;
use nix::sched::CloneFlags;
use nix::sys::stat;

pub fn enter_mnt_namespace(pid: i32) -> Result<()> {
    let mnt_ns_path = format!("/proc/{}/ns/mnt", pid);
    let mnt_ns = open(mnt_ns_path.as_str(), OFlag::O_RDWR, stat::Mode::all())?;

    setns(mnt_ns, CloneFlags::CLONE_NEWNS)?;

    return Ok(());
}
