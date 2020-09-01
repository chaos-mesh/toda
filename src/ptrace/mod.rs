use anyhow::{anyhow, Result};
use nix::sys::mman::{MapFlags, ProtFlags};
use nix::sys::ptrace;
use nix::sys::uio::{process_vm_readv, process_vm_writev, IoVec, RemoteIoVec};
use nix::sys::wait;
use nix::unistd::Pid;

use tracing::{info, trace};

#[derive(Debug)]
pub struct TracedProcess {
    pub pid: i32,
}

impl TracedProcess {
    pub fn trace(pid: i32) -> Result<TracedProcess> {
        let raw_pid = pid;
        let pid = Pid::from_raw(pid);

        ptrace::attach(pid)?;
        info!("trace process: {} successfully", pid);

        // TODO: check wait result
        let _ = wait::waitpid(pid, None)?;

        Ok(TracedProcess { pid: raw_pid })
    }
}

impl TracedProcess {
    #[tracing::instrument]
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

    #[tracing::instrument(skip(f))]
    fn with_protect<R, F: Fn(&Self) -> Result<R>>(&self, f: F) -> Result<R> {
        let guard = self.protect()?;

        let ret = f(self)?;

        drop(guard);

        Ok(ret)
    }

    #[tracing::instrument]
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

            let status = wait::waitpid(pid, None)?;
            trace!("wait status: {:?}", status);
            // TODO: check wait result

            let regs = ptrace::getregs(pid)?;

            trace!("returned: {:?}", regs.rax);

            Ok(regs.rax)
        })
    }

    #[tracing::instrument]
    pub fn detach(&self) -> Result<()> {
        info!("detach process: {}", self.pid);
        ptrace::detach(Pid::from_raw(self.pid), None)?;

        Ok(())
    }

    #[tracing::instrument]
    pub fn mmap(&self, length: u64, fd: u64) -> Result<u64> {
        let prot = ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC;
        let flags = MapFlags::MAP_PRIVATE | MapFlags::MAP_ANON;

        self.syscall(
            9,
            &[0, length, prot.bits() as u64, flags.bits() as u64, fd, 0],
        )
    }

    #[tracing::instrument]
    pub fn munmap(&self, addr: u64, len: u64) -> Result<u64> {
        self.syscall(11, &[addr, len])
    }

    #[tracing::instrument(skip(f))]
    pub fn with_mmap<R, F: Fn(&Self, u64) -> Result<R>>(&self, len: u64, f: F) -> Result<R> {
        let addr = self.mmap(len, 0)?;

        let ret = f(self, addr)?;

        self.munmap(addr, len)?;

        Ok(ret)
    }

    #[tracing::instrument]
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

    #[tracing::instrument]
    pub fn read_mem(&self, addr: u64, len: u64) -> Result<Vec<u8>> {
        let pid = Pid::from_raw(self.pid);
        let mut ret = Vec::new();

        process_vm_readv(
            pid,
            &[IoVec::from_mut_slice(ret.as_mut_slice())],
            &[RemoteIoVec {
                base: addr as usize,
                len: len as usize,
            }],
        )?;

        Ok(ret)
    }

    #[tracing::instrument(skip(codes))]
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

                loop {
                    info!("run instructions");
                    ptrace::cont(pid, None)?;

                    info!("wait for pid: {:?}", pid);
                    let status = wait::waitpid(pid, None)?;
                    info!("wait status: {:?}", status);

                    use nix::sys::signal::SIGTRAP;
                    match status {
                        wait::WaitStatus::Stopped(_, SIGTRAP) => {
                            let regs = ptrace::getregs(pid)?;
                            info!("current rip: {:X}", regs.rip);

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
