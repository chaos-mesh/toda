use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::sleep;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};

use super::injector_config::LatencyConfig;
use super::{filter, Injector};
use crate::hookfs::Result;

#[derive(Debug)]
pub struct LatencyInjector {
    latency: Duration,
    filter: filter::Filter,
    cancel_token: CancellationToken,
}

#[async_trait]
impl Injector for LatencyInjector {
    async fn inject(&self, method: &filter::Method, path: &Path) -> Result<()> {
        trace!("test for filter");
        if self.filter.filter(method, path) {
            let token = self.cancel_token.clone();
            let latency = self.latency;
            debug!("inject io delay {:?}", latency);

            select! {
                _ = sleep(latency) => {}
                _ = token.cancelled() => {
                    debug!("cancelled");
                }
            }

            debug!("latency finished");
        }

        Ok(())
    }

    fn interrupt(&self) {
        debug!("interrupt latency");
        self.cancel_token.cancel();
    }
}

impl LatencyInjector {
    pub fn build(conf: LatencyConfig) -> anyhow::Result<Self> {
        trace!("build latency injector");

        Ok(Self {
            latency: conf.latency,
            filter: filter::Filter::build(conf.filter)?,
            cancel_token: CancellationToken::new(),
        })
    }
}
