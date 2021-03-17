use jsonrpc_derive::rpc;
use jsonrpc_stdio_server::{jsonrpc_core::*, ServerBuilder};
use std::sync::{Mutex, mpsc};
use tracing::{info, trace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comm {
    Ok = 0,
    Shutdown = 1,
}

pub async fn start_server(status: anyhow::Result<()>, tx: mpsc::Sender<Comm>) {
    info!("Starting jsonrpc server");
    let server = new_server(status, tx);
    let server = server.build();
    server.await;
}

pub fn new_server(status: anyhow::Result<()>, tx: mpsc::Sender<Comm>) -> ServerBuilder {
    info!("Creating jsonrpc server");
    let io = new_handler(status, tx);
    ServerBuilder::new(io)
}

pub fn new_handler(status: anyhow::Result<()>, tx: mpsc::Sender<Comm>) -> IoHandler {
    info!("Creating jsonrpc handler");
    let mut io = IoHandler::new();
    io.extend_with(RpcImpl { status, tx:Mutex::new(tx) }.to_delegate());
    io
}

#[rpc]
pub trait Rpc {
    #[rpc(name = "get_status")]
    fn get_status(&self) -> Result<String>;
}

pub struct RpcImpl {
    status: anyhow::Result<()>,
    tx: Mutex<mpsc::Sender<Comm>>,
}

impl Drop for RpcImpl {
    fn drop(&mut self) {
        println!("> Dropping RpcImpl");
    }
}

impl Rpc for RpcImpl {
    fn get_status(&self) -> Result<String> {
        trace!("rpc get_status called");
        match &self.status {
            Ok(_) => Ok("ok".to_string()),
            Err(e) => {
                let tx = &self.tx.lock().unwrap();
                tx.send(Comm::Shutdown).expect("Send through channel failed");
                Ok(e.to_string())
            },
        }
    }
}
