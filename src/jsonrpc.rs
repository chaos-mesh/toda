use std::sync::{mpsc, Arc, Mutex};

use jsonrpc_derive::rpc;
use jsonrpc_stdio_server::jsonrpc_core::*;
use jsonrpc_stdio_server::ServerBuilder;
use tracing::{info, trace};

use crate::hookfs::HookFs;
use crate::injector::{InjectorConfig, MultiInjector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comm {
    Shutdown = 0,
}

pub async fn start_server(config: RpcImpl) {
    info!("Starting jsonrpc server");
    let server = new_server(config);
    let server = server.build();
    server.await;
}

pub fn new_server(config: RpcImpl) -> ServerBuilder {
    info!("Creating jsonrpc server");
    let io = new_handler(config);
    ServerBuilder::new(io)
}

pub fn new_handler(config: RpcImpl) -> IoHandler {
    info!("Creating jsonrpc handler");
    let mut io = IoHandler::new();
    io.extend_with(config.to_delegate());
    io
}

#[rpc]
pub trait Rpc {
    #[rpc(name = "get_status")]
    fn get_status(&self, inst: String) -> Result<String>;
    #[rpc(name = "update")]
    fn update(&self, config: Vec<InjectorConfig>) -> Result<String>;
}

pub struct RpcImpl {
    status: Mutex<anyhow::Result<()>>,
    tx: Mutex<mpsc::Sender<Comm>>,
    hookfs: Option<Arc<HookFs>>,
}

impl RpcImpl {
    pub fn new(
        status: Mutex<anyhow::Result<()>>,
        tx: Mutex<mpsc::Sender<Comm>>,
        hookfs: Option<Arc<HookFs>>,
    ) -> Self {
        Self { status, tx, hookfs }
    }
}

impl Drop for RpcImpl {
    fn drop(&mut self) {
        trace!("Dropping jrpc handler");
    }
}

impl Rpc for RpcImpl {
    fn get_status(&self, _inst: String) -> Result<String> {
        info!("rpc get_status called");
        match &*self.status.lock().unwrap() {
            Ok(_) => Ok("ok".to_string()),
            Err(e) => {
                let tx = &self.tx.lock().unwrap();
                tx.send(Comm::Shutdown)
                    .expect("Send through channel failed");
                Ok(e.to_string())
            }
        }
    }
    fn update(&self, config: Vec<InjectorConfig>) -> Result<String> {
        info!("rpc update called");
        if let Err(e) = &*self.status.lock().unwrap() {
            return Ok(e.to_string());
        }
        let injectors = MultiInjector::build(config);
        if let Err(e) = &injectors {
            return Ok(e.to_string());
        }
        futures::executor::block_on((async || {
            let hookfs = self.hookfs.as_ref().unwrap();
            let mut current_injectors = hookfs.injector.write().await;
            *current_injectors = injectors.unwrap();
        })());
        Ok("ok".to_string())
    }
}
