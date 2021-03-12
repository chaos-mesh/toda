use jsonrpc_stdio_server::{ServerBuilder, jsonrpc_core::*};
use jsonrpc_derive::rpc;
use tracing::info;

pub async fn start_server() {
    info!("Starting jsonrpc server");
    let server = new_server();
    let server = server.build();
    server.await;
}

pub fn new_server() -> ServerBuilder {
    let io = new_handler();
    return ServerBuilder::new(io)
}

pub fn new_handler() -> IoHandler {
    let mut io = IoHandler::new();
    io.extend_with(RpcImpl.to_delegate());
    io
}

#[rpc]
pub trait Rpc {
	#[rpc(name = "ping")]
	fn ping(&self) -> Result<String>;
}

pub struct RpcImpl;
impl Rpc for RpcImpl {
	fn ping(&self) -> Result<String> {
		Ok("pong".to_string())
	}
}