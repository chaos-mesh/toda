mod multi_injector;
mod latency_injector;
mod fault_injector;
mod attr_override_injector;
mod filter;
mod injector_config;

pub use injector_config::InjectorConfig;
pub use multi_injector::MultiInjector;
pub use filter::Method;

use async_trait::async_trait;
use crate::hookfs::{Result, Reply};

use std::path::Path;

#[async_trait]
pub trait Injector: Send + Sync + std::fmt::Debug {
    async fn inject(&self, method: &filter::Method, path: &Path) -> Result<()>;

    fn inject_reply(&self, method: &filter::Method, path: &Path, reply: &mut Reply) -> Result<()>;
}

default impl<T> Injector for T
where T: Send + Sync + std::fmt::Debug {
    default fn inject_reply(&self, _: &filter::Method, _: &Path, _: &mut Reply) -> Result<()> {
        Ok(())
    }
}