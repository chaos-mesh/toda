mod multi_injector;
mod latency_injector;
mod fault_injector;
mod filter;
mod injector_config;

pub use injector_config::InjectorConfig;
pub use multi_injector::MultiInjector;
pub use filter::Method;

use async_trait::async_trait;
use crate::hookfs::Result;

use std::path::Path;

#[async_trait]
pub trait Injector: Send + Sync + std::fmt::Debug {
    async fn inject(&self, method: &filter::Method, path: &Path) -> Result<()>;
}