use jsonrpc_stdio_server::{ServerBuilder, jsonrpc_core::*};
use tracing::{info, trace};

pub async fn start_server() {
    info!("Starting jsonrpc server");
    let server = new_server();
    let server = server.build();
    server.await;
}

pub fn new_server() -> ServerBuilder {
    let mut io = IoHandler::default();
    handle_ping(&mut io);
    let request = r#"{"jsonrpc": "2.0", "method": "ping", "params": {}, "id": 1}"#;
	let response = r#"{"jsonrpc":"2.0","result":"pong","id":1}"#;
    assert_eq!(io.handle_request_sync(request), Some(response.to_string()));
    return ServerBuilder::new(io)
}

fn handle_ping(handler:&mut IoHandler) {
    handler.add_sync_method("ping", |_params|{
        trace!("Receiving ping");
        Ok(Value::String("pong".to_owned()))
    });
}