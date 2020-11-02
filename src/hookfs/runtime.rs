use once_cell::sync::Lazy;

use nix::sys::signal::{signal, SigHandler, Signal};
use nix::sys::wait;
use nix::unistd::Pid;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use std::future::Future;
use std::sync::RwLock;

use log::{trace, info, warn};

pub static RUNTIME: Lazy<RwLock<Option<Runtime>>> = Lazy::new(|| {
    trace!("build tokio runtime");

    RwLock::new(Some(
        tokio::runtime::Builder::new()
            .threaded_scheduler()
            .thread_name("toda")
            .on_thread_start(|| {
                if let Err(err) = unsafe { signal(Signal::SIGCHLD, SigHandler::SigIgn) } {
                    warn!("fail to set signal handler: {:?}", err);
                };
                trace!("thread started");
            })
            .on_thread_stop(|| {
                trace!("thread stopping");
                info!("wait for subprocess to die");
                // TODO: figure out why it panics here
                if let Err(err) = wait::waitpid(Pid::from_raw(0), None) {
                    warn!("fail to wait subprocess: {:?}", err)
                }
            })
            .enable_all()
            .build()
            .unwrap(),
    ))
});

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    if let Some(runtime) = &*RUNTIME.read().unwrap() {
        return runtime.spawn(future);
    }
    unreachable!()
}

pub fn spawn_blocking<F, R>(func: F) -> JoinHandle<R>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    if let Some(runtime) = &*RUNTIME.read().unwrap() {
        return runtime.handle().spawn_blocking(func);
    }
    unreachable!()
}
