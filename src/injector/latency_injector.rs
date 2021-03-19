use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::delay_for;
use tracing::{debug, trace};

use super::injector_config::LatencyConfig;
use super::{filter, Injector};
use crate::hookfs::Result;

#[derive(Debug)]
pub struct LatencyInjector {
    latency: Duration,
    filter: filter::Filter,
}

#[async_trait]
impl Injector for LatencyInjector {
    async fn inject(&self, method: &filter::Method, path: &Path) -> Result<()> {
        trace!("test for filter");
        if self.filter.filter(method, path) {
            debug!("inject io delay {:?}", self.latency);
            delay_for(self.latency).await;
            debug!("latency finished");
        }

        Ok(())
    }
}

impl LatencyInjector {
    pub fn build(conf: LatencyConfig) -> anyhow::Result<Self> {
        trace!("build latency injector");

        Ok(Self {
            latency: conf.latency,
            filter: filter::Filter::build(conf.filter)?,
        })
    }
}
