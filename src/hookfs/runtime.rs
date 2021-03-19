use std::future::Future;
use std::sync::RwLock;

use once_cell::sync::Lazy;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tracing::trace;

pub static RUNTIME: Lazy<RwLock<Option<Runtime>>> = Lazy::new(|| {
    trace!("build tokio runtime");

    RwLock::new(Some(
        tokio::runtime::Builder::new()
            .threaded_scheduler()
            .thread_name("toda")
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
