mod fault_injector;
mod filter;
mod injector_config;
mod latency_injector;
mod multi_injector;

pub use filter::Method;
pub use injector_config::InjectorConfig;
pub use multi_injector::MultiInjector;

use crate::hookfs::Result;
use async_trait::async_trait;

use std::path::Path;

#[async_trait]
pub trait Injector: Send + Sync + std::fmt::Debug {
    async fn inject(&self, method: &filter::Method, path: &Path) -> Result<()>;
}
