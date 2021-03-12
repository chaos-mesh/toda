use toda::jsonrpc::new_handler;
#[test]
fn test_ping() {
    let io = new_handler();
    let request = r#"{"jsonrpc": "2.0","method":"ping","id":1}"#;
    let response = r#"{"jsonrpc":"2.0","result":"pong","id":1}"#;
    assert_eq!(io.handle_request_sync(request), Some(response.to_string()));
}
