use std::sync::Arc;
use std::sync::{Condvar, Mutex};

struct Stop {
    inner: Mutex<bool>,
    condvar: Condvar,
}

impl Stop {
    fn new() -> Stop {
        Stop {
            inner: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }
    fn wait(&self) {
        let mut inner = self.inner.lock().unwrap();
        while !*inner {
            inner = self.condvar.wait(inner).unwrap();
        }
    }
    fn wake(&self) {
        let mut inner = self.inner.lock().unwrap();

        *inner = true;
        self.condvar.notify_one();
    }
}

pub struct StopGuard {
    stop: Arc<Stop>,
}

impl StopGuard {
    fn new(stop: Arc<Stop>) -> StopGuard {
        StopGuard { stop }
    }
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.stop.wake()
    }
}

pub struct StopWaiter {
    stop: Arc<Stop>,
}

impl StopWaiter {
    fn new(stop: Arc<Stop>) -> StopWaiter {
        StopWaiter { stop }
    }

    pub fn wait(&self) {
        self.stop.wait()
    }
}

pub fn lock() -> (StopWaiter, StopGuard) {
    let stop = Arc::new(Stop::new());

    (StopWaiter::new(stop.clone()), StopGuard::new(stop.clone()))
}
