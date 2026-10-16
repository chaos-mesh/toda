use std::sync::mpsc::channel;
use std::sync::Mutex;

use anyhow::anyhow;
use toda::jsonrpc::{self, new_handler, Comm};
#[test]
fn test_status_good() {
    let (tx, _rx) = channel();
    let io = new_handler(jsonrpc::RpcImpl::new(
        Mutex::new(Ok(())),
        Mutex::new(tx),
        None,
    ));
    let request = r#"{"jsonrpc": "2.0","method":"get_status","params":[""],"id":1}"#;
    let response = r#"{"jsonrpc":"2.0","result":"ok","id":1}"#;
    assert_eq!(io.handle_request_sync(request), Some(response.to_string()));
}

#[test]
fn test_status_bad() {
    let (tx, rx) = channel();
    let io = new_handler(jsonrpc::RpcImpl::new(
        Mutex::new(Err(anyhow!("Not good"))),
        Mutex::new(tx),
        None,
    ));
    let request = r#"{"jsonrpc": "2.0","method":"get_status","params":[""],"id":1}"#;
    let response = r#"{"jsonrpc":"2.0","result":"Not good","id":1}"#;
    assert_eq!(io.handle_request_sync(request), Some(response.to_string()));
    assert_eq!(rx.recv().unwrap(), Comm::Shutdown);
}

#[test]
fn test_should_not_update_config_if_status_is_failed() {
    let (tx, _rx) = channel();
    let request = r#"{"jsonrpc": "2.0","method":"update","params":[[]],"id":1}"#;
    let response = r#"{"jsonrpc":"2.0","result":"Not good","id":1}"#;
    let io = new_handler(jsonrpc::RpcImpl::new(
        Mutex::new(Err(anyhow!("Not good"))),
        Mutex::new(tx),
        None,
    ));
    assert_eq!(io.handle_request_sync(request), Some(response.to_string()));
}

#[test]
fn test_should_fail_if_config_is_bad() {
    let (tx, _rx) = channel();
    let request = r#"{"jsonrpc": "2.0","method":"update","params":[["blah"]],"id":1}"#;
    let io = new_handler(jsonrpc::RpcImpl::new(
        Mutex::new(Ok(())),
        Mutex::new(tx),
        None,
    ));
    let response = io.handle_request_sync(request).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["error"]["code"], -32602);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("Invalid params:"));
    assert!(response.get("result").is_none());
}
