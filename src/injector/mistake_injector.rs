use async_trait::async_trait;
use rand::Rng;

use std::{
    cmp::{max, min},
    path::Path,
};

use super::filter;
use super::injector_config::MistakeConfig;
use super::injector_config::MistakeType;
use super::injector_config::MistakesConfig;
use super::Injector;
use crate::hookfs::Reply;
use crate::hookfs::Result;

use tracing::{debug, trace};

#[derive(Debug)]
pub struct MistakeInjector {
    mistake: MistakeConfig,
    filter: filter::Filter,
}

#[async_trait]
impl Injector for MistakeInjector {
    async fn inject(&self, _: &filter::Method, _: &Path) -> Result<()> {
        debug!("MI:Injecting");
        Ok(())
    }

    fn inject_reply(&self, method: &super::Method, path: &Path, reply: &mut Reply) -> Result<()> {
        if self.filter.filter(method, path) {
            debug!("MI:Injecting reply");
            if let Reply::Data(data) = reply {
                let data = &mut data.data;
                self.handle(data);
            }
        }
        Ok(())
    }

    fn inject_write_data(&self, path: &Path, data: &mut Vec<u8>) -> Result<()> {
        if self.filter.filter(&super::Method::WRITE, path) {
            debug!("MI:Injecting write data");
            self.handle(data);
        }
        Ok(())
    }
}

impl MistakeInjector {
    pub fn build(conf: MistakesConfig) -> anyhow::Result<Self> {
        trace!("build mistake injector");
        Ok(Self {
            mistake: conf.mistake,
            filter: filter::Filter::build(conf.filter)?,
        })
    }
    pub fn handle(&self, data: &mut Vec<u8>) {
        trace!("sabotage data");
        let mut rng = rand::thread_rng();
        let data_length = data.len();
        let mistake = &self.mistake;
        if rng.gen_range(0, 100) >= mistake.percent {
            continue;
        }
        let occurrence = match mistake.max_occurrences {
            0 => 0,
            mo => rng.gen_range(1, mo + 1),
        };
        for _ in 0..occurrence {
            let pos = rng.gen_range(0, max(data_length, 1));
            let length = match min(mistake.max_length, data_length - pos) {
                0 => 0,
                l => rng.gen_range(1, l + 1),
            };
            debug!(
                "Setting index [{},{}) to {:?}",
                pos,
                pos + length,
                mistake.filling
            );
            match mistake.filling {
                MistakeType::Zero => {
                    for i in pos..pos + length {
                        data[i] = 0;
                    }
                }
                MistakeType::Random => rng.fill(&mut data[pos..pos + length]),
            }
        }
    }
}
