mod attr_override_injector;
mod fault_injector;
mod filter;
mod injector_config;
mod latency_injector;
mod mistake_injector;
mod multi_injector;

pub use filter::Method;
pub use injector_config::InjectorConfig;
pub use multi_injector::MultiInjector;

use crate::hookfs::{Reply, Result};
use async_trait::async_trait;
use fuser::FileAttr;

use std::{path::Path};

#[async_trait]
pub trait Injector: Send + Sync + std::fmt::Debug {
    async fn inject(&self, method: &filter::Method, path: &Path) -> Result<()>;

    fn inject_reply(
        &self,
        _method: &filter::Method,
        _path: &Path,
        _reply: &mut Reply,
    ) -> Result<()> {
        Ok(())
    }
    fn inject_write_data(
        &self,
        _path: &Path,
        _data: &mut Vec<u8>,
    ) -> Result<()> {
        Ok(())
    }

    fn inject_attr(&self, _attr: &mut FileAttr, _path: &Path) {}
}
