use std::sync::mpsc::channel;

use toda::jsonrpc::{Comm, new_handler};
use anyhow::anyhow;
#[test]
fn test_status_good() {
    let (tx, rx) = channel();
    let io = new_handler(Ok(()),tx);
    let request = r#"{"jsonrpc": "2.0","method":"get_status","id":1}"#;
    let response = r#"{"jsonrpc":"2.0","result":"ok","id":1}"#;
    assert_eq!(io.handle_request_sync(request), Some(response.to_string()));
}

#[test]
fn test_status_bad() {
    let (tx, rx) = channel();
    let io = new_handler(Err(anyhow!("Not good")),tx);
    let request = r#"{"jsonrpc": "2.0","method":"get_status","id":1}"#;
    let response = r#"{"jsonrpc":"2.0","result":"Not good","id":1}"#;
    assert_eq!(io.handle_request_sync(request), Some(response.to_string()));
    assert_eq!(rx.recv().unwrap(), Comm::Shutdown);
}

