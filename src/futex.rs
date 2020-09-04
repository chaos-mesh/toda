use std::ptr::null;
use std::sync::atomic::AtomicI32;

use libc::{syscall, SYS_futex, FUTEX_WAIT, FUTEX_WAKE};

use anyhow::Result;
use nix::Error;

pub struct Futex {
    inner: AtomicI32,
}

impl Futex {
    pub fn new() -> Futex {
        Futex {
            inner: AtomicI32::new(0),
        }
    }
    pub fn wait(&self) -> Result<()> {
        let ret = unsafe { syscall(SYS_futex, self.inner.as_mut_ptr(), FUTEX_WAIT, 0, 0, 0, 0) };

        if ret == -1 {
            Err(Error::last().into())
        } else {
            Ok(())
        }
    }
    pub fn wake(&self, nr: i32) -> Result<()> {
        let ret = unsafe { syscall(SYS_futex, self.inner.as_mut_ptr(), FUTEX_WAKE, nr, 0, 0, 0) };

        if ret == -1 {
            Err(Error::last().into())
        } else {
            Ok(())
        }
    }
}
