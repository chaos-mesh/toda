use std::sync::atomic::{AtomicI32, Ordering};

use libc::{syscall, SYS_futex, FUTEX_WAIT, FUTEX_WAKE};

use anyhow::Result;
use nix::Error;

use log::{error, info};

// Don't reuse one futex!
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
        info!("resume from futex");

        if ret == -1 {
            let err = Error::last();
            info!("error while waiting for futex: {:?}", err);
            Err(err.into())
        } else {
            Ok(())
        }
    }
    pub fn wake(&self, nr: i32) -> Result<()> {
        self.inner.store(1, Ordering::SeqCst);
        let ret = unsafe { syscall(SYS_futex, self.inner.as_mut_ptr(), FUTEX_WAKE, nr, 0, 0, 0) };
        info!("wake up futex");

        if ret == -1 {
            let err = Error::last();
            error!("error while waking up futex: {:?}", err);
            Err(err.into())
        } else {
            Ok(())
        }
    }
}
