use super::fault_injector::FaultInjector;
use super::filter;
use super::injector_config::InjectorConfig;
use super::latency_injector::LatencyInjector;
use super::Injector;
use crate::hookfs::Result;

use async_trait::async_trait;
use tracing::trace;

use std::path::Path;

#[derive(Debug)]
pub struct MultiInjector {
    injectors: Vec<Box<dyn Injector>>,
}

impl MultiInjector {
    #[tracing::instrument]
    pub fn build(conf: Vec<InjectorConfig>) -> anyhow::Result<Self> {
        trace!("build multiinjectors");
        let mut injectors = Vec::new();

        for injector in conf.into_iter() {
            let injector = match injector {
                InjectorConfig::Faults(faults) => {
                    (box FaultInjector::build(faults)?) as Box<dyn Injector>
                }
                InjectorConfig::Latency(latency) => {
                    (box LatencyInjector::build(latency)?) as Box<dyn Injector>
                }
            };
            injectors.push(injector)
        }

        return Ok(Self { injectors });
    }
}

#[async_trait]
impl Injector for MultiInjector {
    #[tracing::instrument]
    async fn inject(&self, method: &filter::Method, path: &Path) -> Result<()> {
        for injector in self.injectors.iter() {
            injector.inject(method, path).await?
        }

        return Ok(());
    }
}
