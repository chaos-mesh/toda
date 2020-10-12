use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use libc::{syscall, SYS_futex, FUTEX_WAIT, FUTEX_WAKE};

use anyhow::Result;
use nix::Error;

use log::{error, info};

// Don't reuse one futex!
struct Futex {
    inner: AtomicI32,
}

impl Futex {
    fn new() -> Futex {
        Futex {
            inner: AtomicI32::new(0),
        }
    }
    fn wait(&self) -> Result<()> {
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
    fn wake(&self, nr: i32) -> Result<()> {
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

pub struct FutexGuard {
    futex: Arc<Futex>,
}

impl FutexGuard {
    fn new(futex: Arc<Futex>) -> FutexGuard {
        FutexGuard { futex }
    }
}

impl Drop for FutexGuard {
    fn drop(&mut self) {
        self.futex.wake(1).unwrap()
    }
}

pub struct FutexWaiter {
    futex: Arc<Futex>,
}

impl FutexWaiter {
    fn new(futex: Arc<Futex>) -> FutexWaiter {
        FutexWaiter { futex }
    }

    pub fn wait(&self) -> Result<()> {
        self.futex.wait()
    }
}

pub fn lock() -> (FutexWaiter, FutexGuard) {
    let futex = Arc::new(Futex::new());

    (
        FutexWaiter::new(futex.clone()),
        FutexGuard::new(futex.clone()),
    )
}
