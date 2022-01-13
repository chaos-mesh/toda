use anyhow::{anyhow, Result};
use nix::sys::ptrace;
use nix::sys::signal::Signal;
use nix::sys::uio::{process_vm_writev, IoVec, RemoteIoVec};
use nix::sys::wait;
use nix::unistd::Pid;
use nix::{
    errno::Errno,
    sys::mman::{MapFlags, ProtFlags},
    Error::Sys,
};
use Error::Internal;

use procfs::{process::Task, ProcError};
use retry::{
    delay::Fixed,
    Error::{self, Operation},
    OperationResult,
};
use tracing::{error, info, instrument, trace, warn};

use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::{cell::RefCell, collections::HashSet};

// There should be only one PtraceManager in one thread. But as we don't implement TLS
// , we cannot use thread-local variables safely.
#[derive(Debug, Default)]
pub struct PtraceManager {
    counter: RefCell<HashMap<i32, i32>>,
}

thread_local! {
    static PTRACE_MANAGER: PtraceManager = PtraceManager::default()
}

pub fn trace(pid: i32) -> Result<TracedProcess> {
    PTRACE_MANAGER.with(|pm| pm.trace(pid))
}

fn thread_is_gone(state: char) -> bool {
    // return true if the process is Zombie or Dead
    state == 'Z' || state == 'x' || state == 'X'
}

#[instrument]
fn attach_task(task: &Task) -> Result<()> {
    let pid = Pid::from_raw(task.tid);
    let process = procfs::process::Process::new(task.tid)?;

    trace!("attach task: {}", task.tid);
    match ptrace::attach(pid) {
        Err(Sys(errno))
            if errno == Errno::ESRCH
                || (errno == Errno::EPERM && thread_is_gone(process.stat.state)) =>
        {
            info!("task {} doesn't exist, maybe has stopped", task.tid)
        }
        Err(err) => {
            warn!("attach error: {:?}", err);
            return Err(err.into());
        }
        _ => {}
    }
    info!("attach task: {} successfully", task.tid);

    // TODO: check wait result
    match wait::waitpid(pid, Some(wait::WaitPidFlag::__WALL)) {
        Ok(status) => {
            info!("wait status: {:?}", status);
        }
        Err(err) => warn!("fail to wait for process({}): {:?}", pid, err),
    };

    Ok(())
}

impl PtraceManager {
    #[instrument(skip(self))]
    pub fn trace(&self, pid: i32) -> Result<TracedProcess> {
        let raw_pid = pid;
        let pid = Pid::from_raw(pid);

        let mut counter_ref = self.counter.borrow_mut();
        match counter_ref.get_mut(&raw_pid) {
            Some(count) => *count += 1,
            None => {
                trace!("stop {} successfully", pid);

                let mut iterations = 2;
                let mut traced_tasks = HashSet::<i32>::new();

                while iterations > 0 {
                    let mut new_threads_found = false;
                    let process = procfs::process::Process::new(raw_pid)?;
                    for task in (process.tasks()?).flatten() {
                        if traced_tasks.contains(&task.tid) {
                            continue;
                        }

                        if let Ok(()) = attach_task(&task) {
                            trace!("newly traced task: {}", task.tid);
                            new_threads_found = true;
                            traced_tasks.insert(task.tid);
                        }
                    }

                    if !new_threads_found {
                        iterations -= 1;
                    }
                }

                info!("trace process: {} successfully", pid);
                counter_ref.insert(raw_pid, 1);
            }
        }

        Ok(TracedProcess { pid: raw_pid })
    }

    #[instrument(skip(self))]
    pub fn detach(&self, pid: i32) -> Result<()> {
        let mut counter_ref = self.counter.borrow_mut();
        match counter_ref.get_mut(&pid) {
            Some(count) => {
                *count -= 1;
                trace!("decrease counter to {}", *count);
                if *count < 1 {
                    counter_ref.remove(&pid);

                    info!("detach process: {}", pid);
                    if let Err(err) = retry::retry::<_, _, _, anyhow::Error, _>(
                        Fixed::from_millis(500).take(20),
                        || match procfs::process::Process::new(pid) {
                            Err(ProcError::NotFound(_)) => {
                                info!("process {} not found", pid);
                                OperationResult::Ok(())
                            }
                            Err(err) => {
                                warn!("fail to detach task: {}, retry", pid);
                                OperationResult::Retry(err.into())
                            }
                            Ok(process) => match process.tasks() {
                                Err(err) => OperationResult::Retry(err.into()),
                                Ok(tasks) => {
                                    for task in tasks.flatten() {
                                        match ptrace::detach(Pid::from_raw(task.tid), None) {
                                                Ok(()) => {
                                                    info!("successfully detached task: {}", task.tid);
                                                }
                                                Err(Sys(Errno::ESRCH)) => trace!(
                                                    "task {} doesn't exist, maybe has stopped or not traced",
                                                    task.tid
                                                ),
                                                Err(err) => {
                                                    warn!("fail to detach: {:?}", err)
                                                },
                                            }
                                        trace!("detach task: {} successfully", task.tid);
                                    }
                                    info!("detach process: {} successfully", pid);
                                    OperationResult::Ok(())
                                }
                            },
                        },
                    ) {
                        warn!("fail to detach: {:?}", err);
                        match err {
                            Operation {
                                error: e,
                                total_delay: _,
                                tries: _,
                            } => return Err(e),
                            Internal(err) => {
                                error!("internal error: {:?}", err)
                            }
                        }
                    };
                }

                Ok(())
            }
            None => Err(anyhow::anyhow!("haven't traced this process")),
        }
    }
}

#[derive(Debug)]
pub struct TracedProcess {
    pub pid: i32,
}

impl Clone for TracedProcess {
    fn clone(&self) -> Self {
        // TODO: handler error here
        PTRACE_MANAGER.with(|pm| pm.trace(self.pid)).unwrap()
    }
}

impl TracedProcess {
    #[instrument]
    fn protect(&self) -> Result<ThreadGuard> {
        let regs = ptrace::getregs(Pid::from_raw(self.pid))?;

        let rip = regs.rip;
        trace!("protecting regs: {:?}", regs);
        let rip_ins = ptrace::read(Pid::from_raw(self.pid), rip as *mut libc::c_void)?;

        let guard = ThreadGuard {
            tid: self.pid,
            regs,
            rip_ins,
        };
        Ok(guard)
    }

    #[instrument(skip(f))]
    fn with_protect<R, F: Fn(&Self) -> Result<R>>(&self, f: F) -> Result<R> {
        let guard = self.protect()?;

        let ret = f(self)?;

        drop(guard);

        Ok(ret)
    }

    #[instrument]
    fn syscall(&self, id: u64, args: &[u64]) -> Result<u64> {
        trace!("run syscall {} {:?}", id, args);

        self.with_protect(|thread| -> Result<u64> {
            let pid = Pid::from_raw(thread.pid);

            let mut regs = ptrace::getregs(pid)?;
            let cur_ins_ptr = regs.rip;

            regs.rax = id;
            for (index, arg) in args.iter().enumerate() {
                // All these registers are hard coded for x86 platform
                if index == 0 {
                    regs.rdi = *arg
                } else if index == 1 {
                    regs.rsi = *arg
                } else if index == 2 {
                    regs.rdx = *arg
                } else if index == 3 {
                    regs.r10 = *arg
                } else if index == 4 {
                    regs.r8 = *arg
                } else if index == 5 {
                    regs.r9 = *arg
                } else {
                    return Err(anyhow!("too many arguments for a syscall"));
                }
            }
            trace!("setting regs for pid: {:?}, regs: {:?}", pid, regs);
            ptrace::setregs(pid, regs)?;

            // We only support x86-64 platform now, so using hard coded `LittleEndian` here is ok.
            unsafe {
                ptrace::write(
                    pid,
                    cur_ins_ptr as *mut libc::c_void,
                    0x050f as *mut libc::c_void,
                )?
            };
            ptrace::step(pid, None)?;

            loop {
                let status = wait::waitpid(pid, None)?;
                info!("wait status: {:?}", status);
                match status {
                    wait::WaitStatus::Stopped(_, Signal::SIGTRAP) => break,
                    _ => ptrace::step(pid, None)?,
                }
            }

            let regs = ptrace::getregs(pid)?;

            trace!("returned: {:?}", regs.rax);

            Ok(regs.rax)
        })
    }

    #[instrument]
    pub fn mmap(&self, length: u64, fd: u64) -> Result<u64> {
        let prot = ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC;
        let flags = MapFlags::MAP_PRIVATE | MapFlags::MAP_ANON;

        self.syscall(
            9,
            &[0, length, prot.bits() as u64, flags.bits() as u64, fd, 0],
        )
    }

    #[instrument]
    pub fn munmap(&self, addr: u64, len: u64) -> Result<u64> {
        self.syscall(11, &[addr, len])
    }

    #[instrument(skip(f))]
    pub fn with_mmap<R, F: Fn(&Self, u64) -> Result<R>>(&self, len: u64, f: F) -> Result<R> {
        let addr = self.mmap(len, 0)?;

        let ret = f(self, addr)?;

        self.munmap(addr, len)?;

        Ok(ret)
    }

    #[instrument]
    pub fn chdir<P: AsRef<Path> + std::fmt::Debug>(&self, filename: P) -> Result<()> {
        let filename = CString::new(filename.as_ref().as_os_str().as_bytes())?;
        let path = filename.as_bytes_with_nul();

        self.with_mmap(path.len() as u64, |process, addr| {
            process.write_mem(addr, path)?;

            self.syscall(80, &[addr])?;
            Ok(())
        })
    }

    #[instrument]
    pub fn write_mem(&self, addr: u64, content: &[u8]) -> Result<()> {
        let pid = Pid::from_raw(self.pid);

        process_vm_writev(
            pid,
            &[IoVec::from_slice(content)],
            &[RemoteIoVec {
                base: addr as usize,
                len: content.len(),
            }],
        )?;

        Ok(())
    }

    #[instrument(skip(codes))]
    pub fn run_codes<F: Fn(u64) -> Result<(u64, Vec<u8>)>>(&self, codes: F) -> Result<()> {
        let pid = Pid::from_raw(self.pid);

        let regs = ptrace::getregs(pid)?;
        let (_, ins) = codes(regs.rip)?; // generate codes to get length

        self.with_mmap(ins.len() as u64 + 16, |_, addr| {
            self.with_protect(|_| {
                let (offset, ins) = codes(addr)?; // generate codes

                let end_addr = addr + ins.len() as u64;
                trace!("write instructions to addr: {:X}-{:X}", addr, end_addr);
                self.write_mem(addr, &ins)?;

                let mut regs = ptrace::getregs(pid)?;
                trace!("modify rip to addr: {:X}", addr + offset);
                regs.rip = addr + offset;
                ptrace::setregs(pid, regs)?;

                let regs = ptrace::getregs(pid)?;
                info!("current registers: {:?}", regs);

                loop {
                    info!("run instructions");
                    ptrace::cont(pid, None)?;

                    info!("wait for pid: {:?}", pid);
                    let status = wait::waitpid(pid, None)?;
                    info!("wait status: {:?}", status);

                    use nix::sys::signal::SIGTRAP;
                    let regs = ptrace::getregs(pid)?;

                    info!("current registers: {:?}", regs);
                    match status {
                        wait::WaitStatus::Stopped(_, SIGTRAP) => {
                            break;
                        }
                        _ => info!("continue running replacers"),
                    }
                }
                Ok(())
            })
        })
    }
}

impl Drop for TracedProcess {
    fn drop(&mut self) {
        trace!("dropping traced process: {}", self.pid);

        if let Err(err) = PTRACE_MANAGER.with(|pm| pm.detach(self.pid)) {
            info!(
                "detaching process {} failed with error: {:?}",
                self.pid, err
            )
        }
    }
}

#[derive(Debug)]
struct ThreadGuard {
    tid: i32,
    regs: libc::user_regs_struct,
    rip_ins: i64,
}

impl Drop for ThreadGuard {
    fn drop(&mut self) {
        let pid = Pid::from_raw(self.tid);
        unsafe {
            ptrace::write(
                pid,
                self.regs.rip as *mut libc::c_void,
                self.rip_ins as *mut libc::c_void,
            )
            .unwrap();
        }
        ptrace::setregs(pid, self.regs).unwrap();
    }
}
