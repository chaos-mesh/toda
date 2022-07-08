use std::sync::{mpsc, Arc, Mutex};

use tracing::{info};

use crate::hookfs::HookFs;
use crate::injector::{InjectorConfig, MultiInjector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comm {
    Shutdown = 0,
}

#[derive(Debug)]
pub struct TodaRpc {
    status: Mutex<anyhow::Result<()>>,
    tx: Mutex<mpsc::Sender<Comm>>,
    hookfs: Option<Arc<HookFs>>,
}

impl TodaRpc {
    pub fn new(
        status: Mutex<anyhow::Result<()>>,
        tx: Mutex<mpsc::Sender<Comm>>,
        hookfs: Option<Arc<HookFs>>,
    ) -> Self {
        Self { status, tx, hookfs }
    }

    pub fn get_status(&self) -> anyhow::Result<String> {
        info!("rpc get_status called");
        match &*self.status.lock().unwrap() {
            Ok(_) => Ok("ok".to_string()),
            Err(e) => {
                let tx = &self.tx.lock().unwrap();
                tx.send(Comm::Shutdown)
                    .expect("Send through channel failed");
                tracing::error!("get_status error: {:?}", e);
                Ok(e.to_string())
            }
        }
    }
    pub fn update(&self, config: Vec<InjectorConfig>) -> anyhow::Result<String> {
        info!("rpc update called");
        if let Err(e) = &*self.status.lock().unwrap() {
            tracing::error!("update error: {:?}", e);
            return Ok(e.to_string());
        }
        let injectors = MultiInjector::build(config);
        if let Err(e) = &injectors {
            tracing::error!("update MultiInjector::build error: {:?}", e);
            return Ok(e.to_string());
        }
        futures::executor::block_on(async {
            let hookfs = self.hookfs.as_ref().unwrap();
            let mut current_injectors = hookfs.injector.write().await;
            *current_injectors = injectors.unwrap();
        });
        Ok("ok".to_string())
    }
}


