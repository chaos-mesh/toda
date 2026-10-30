use anyhow::{ensure, Result};

use super::{regset, request};

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub(super) struct Registers {
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
}

#[cfg(target_arch = "x86_64")]
pub(super) type Registers = libc::user_regs_struct;

#[cfg(target_arch = "aarch64")]
pub(super) const SYSCALL: &[u8] = &[0x01, 0x00, 0x00, 0xd4];
#[cfg(target_arch = "x86_64")]
pub(super) const SYSCALL: &[u8] = &[0x0f, 0x05];
#[cfg(target_arch = "aarch64")]
pub(super) const AUDIT_ARCH: u32 = 0xc00000b7;
#[cfg(target_arch = "x86_64")]
pub(super) const AUDIT_ARCH: u32 = 0xc000003e;

pub(super) fn get(pid: i32) -> Result<Registers> {
    let mut regs: Registers = unsafe { std::mem::zeroed() };
    regset(pid, libc::PTRACE_GETREGSET, 1, &mut regs)?;
    #[cfg(target_arch = "aarch64")]
    ensure!(
        regs.pstate & 0x10 == 0,
        "32-bit ARM targets are unsupported"
    );
    #[cfg(target_arch = "x86_64")]
    ensure!(regs.cs == 0x33, "32-bit x86 targets are unsupported");
    Ok(regs)
}

pub(super) fn set(pid: i32, mut regs: Registers) -> Result<()> {
    regset(pid, libc::PTRACE_SETREGSET, 1, &mut regs)
}

pub(super) fn pc(regs: &Registers) -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        regs.pc
    }
    #[cfg(target_arch = "x86_64")]
    {
        regs.rip
    }
}

pub(super) fn set_pc(regs: &mut Registers, address: u64) {
    #[cfg(target_arch = "aarch64")]
    {
        regs.pc = address;
    }
    #[cfg(target_arch = "x86_64")]
    {
        regs.rip = address;
    }
}

pub(super) fn call(regs: &mut Registers, number: u64, args: [u64; 6]) {
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[..6].copy_from_slice(&args);
        regs.regs[8] = number;
    }
    #[cfg(target_arch = "x86_64")]
    {
        regs.rax = number;
        regs.rdi = args[0];
        regs.rsi = args[1];
        regs.rdx = args[2];
        regs.r10 = args[3];
        regs.r8 = args[4];
        regs.r9 = args[5];
        regs.orig_rax = u64::MAX;
    }
}

pub(super) fn substitute(pid: i32, mut regs: Registers, number: u64) -> Result<()> {
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[8] = number;
    }
    #[cfg(target_arch = "x86_64")]
    {
        regs.orig_rax = number;
        regs.rax = number;
    }
    set(pid, regs)?;
    syscall_number(pid, number as i32)
}

pub(super) fn syscall_number(pid: i32, number: i32) -> Result<()> {
    #[cfg(target_arch = "aarch64")]
    {
        let mut number = number;
        regset(pid, libc::PTRACE_SETREGSET, 0x404, &mut number)?;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let _ = (pid, number);
    }
    Ok(())
}

pub(super) fn result(regs: &Registers) -> i64 {
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[0] as i64
    }
    #[cfg(target_arch = "x86_64")]
    {
        regs.rax as i64
    }
}

pub(super) fn restart_at_previous_instruction(regs: &Registers) -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        let _ = regs;
        false
    }
    #[cfg(target_arch = "x86_64")]
    {
        regs.orig_rax != u64::MAX && matches!(regs.rax as i64, -512 | -513 | -514 | -516)
    }
}

pub(super) fn replay(entry: Registers, initial: &Registers) -> Registers {
    let mut regs = entry;
    set_pc(&mut regs, pc(&entry) - SYSCALL.len() as u64);
    #[cfg(target_arch = "aarch64")]
    {
        regs.regs[7] = initial.regs[7];
    }
    #[cfg(target_arch = "x86_64")]
    {
        regs.rax = entry.orig_rax;
        regs.orig_rax = u64::MAX;
        let _ = initial;
    }
    regs
}

pub(super) fn block_signals(pid: i32) -> Result<u64> {
    let mut original = 0u64;
    request(
        libc::PTRACE_GETSIGMASK,
        pid,
        8,
        &mut original as *mut _ as usize,
    )?;
    let blocked = u64::MAX;
    request(
        libc::PTRACE_SETSIGMASK,
        pid,
        8,
        &blocked as *const _ as usize,
    )?;
    Ok(original)
}

pub(super) fn restore_signals(pid: i32, mask: u64) -> Result<()> {
    request(libc::PTRACE_SETSIGMASK, pid, 8, &mask as *const _ as usize)?;
    Ok(())
}
