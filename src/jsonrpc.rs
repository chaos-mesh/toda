use jsonrpc_stdio_server::{ServerBuilder, jsonrpc_core::*};
use tracing::info;

pub async fn start_server() {
    info!("Starting jsonrpc server");
    let server = new_server();
    server.build().await;
}

pub fn new_server() -> ServerBuilder {
    let mut io = IoHandler::default();
    handle_ping(&mut io);
    return ServerBuilder::new(io)
}

fn handle_ping(handler:&mut IoHandler) {
    handler.add_sync_method("ping", |_params|{
        Ok(Value::String("pong".to_owned()))
    });
}