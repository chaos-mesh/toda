use anyhow::{anyhow, Result};

use nix::fcntl::{open, OFlag};
use nix::sched::setns;
use nix::sched::CloneFlags;
use nix::sys::{stat, wait};
use nix::unistd::Pid;
use nix::Error;

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

use log::info;

pub fn enter_mnt_namespace(pid: i32) -> Result<()> {
    let mnt_ns_path = format!("/proc/{}/ns/mnt", pid);
    let mnt_ns = open(mnt_ns_path.as_str(), OFlag::O_RDONLY, stat::Mode::all())?;

    setns(mnt_ns, CloneFlags::CLONE_NEWNS)?;

    Ok(())
}

pub struct JoinHandler<R> {
    pid: i32,
    result: Arc<AtomicPtr<Option<R>>>,
    _stack: Vec<u8>,
}

impl<R> JoinHandler<R> {
    pub fn join(&self) -> Result<R> {
        let status = wait::waitpid(Pid::from_raw(self.pid), None)?;
        info!("process {} stopped with status: {:?}", self.pid, status);

        // FIXME: if the cloned process exited/crashed, this process can never resume
        // A possible solution is to wait this process and load the atomic ptr. If this
        // pointer returns None, then we can return an error (or pack the return value with
        // Option<Result<R>>)
        let ret = self.result.load(Ordering::Acquire);
        unsafe {
            if let Some(ret) = (&mut *ret).take() {
                info!("clone returned {}", self.pid);

                Ok(ret)
            } else {
                Err(anyhow!("subprocess exited unexpectedly"))
            }
        }
    }
}

extern "C" fn callback<F: FnOnce() -> Result<R>, R>(args: *mut libc::c_void) -> libc::c_int {
    let args = unsafe {
        Box::from_raw(args as *mut (Box<F>, Arc<AtomicPtr<Option<Result<R>>>>, Box<i32>))
    };
    let (f, ret_ptr, pid) = *args;

    if let Err(err) = enter_mnt_namespace(*pid) {
        let ret = Box::new(Some(Err(err)));
        ret_ptr.store(Box::leak(ret), Ordering::Release)
    }

    let ret = Box::new(Some(f()));
    let ret = Box::leak(ret) as *mut Option<Result<R>>;
    info!("setting result");
    ret_ptr.store(ret, Ordering::Release);

    unsafe { libc::exit(0) }
}

pub fn with_mnt_pid_namespace<F: FnOnce() -> Result<R>, R>(
    f: Box<F>,
    pid: i32,
) -> Result<JoinHandler<Result<R>>> {
    // FIXME: memory leak here
    let ret = Box::new(None);
    let ret_ptr: Arc<AtomicPtr<Option<Result<R>>>> = Arc::new(AtomicPtr::new(Box::leak(ret)));

    let args = Box::new((f, ret_ptr.clone(), Box::new(pid)));

    let mut stack = vec![0u8; 1024 * 1024];

    let pid_ns_path = format!("/proc/{}/ns/pid", pid);
    let pid_ns = open(pid_ns_path.as_str(), OFlag::O_RDONLY, stat::Mode::all())?;

    setns(pid_ns, CloneFlags::CLONE_NEWPID)?;

    let clone_flags = libc::CLONE_VM | libc::CLONE_FILES;

    let pid = unsafe {
        libc::clone(
            callback::<F, R>,
            (stack.as_mut_ptr() as *mut libc::c_void).add(1024 * 1024),
            clone_flags | libc::SIGCHLD,
            Box::into_raw(args) as *mut libc::c_void,
        )
    };
    if pid == -1 {
        return Err(Error::last().into());
    }

    Ok(JoinHandler {
        pid,
        _stack: stack,
        result: ret_ptr,
    })
}
