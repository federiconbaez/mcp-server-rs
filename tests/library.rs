//! Library-level checks that need custom tools or direct access.

use mcp_server::tools::{Tool, ToolContext, ToolResult};
use mcp_server::{Conn, Server};
use serde_json::{json, Value};

struct Panics;

impl Tool for Panics {
    fn name(&self) -> &str {
        "boom"
    }
    fn definition(&self) -> Value {
        json!({"name":"boom","inputSchema":{"type":"object"}})
    }
    fn call(&self, _: &Value, _: &ToolContext) -> ToolResult {
        panic!("intentional test panic")
    }
}

fn init(server: &Server, conn: &mut Conn) {
    let r = server
        .handle_bytes(conn, br#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}"#)
        .unwrap();
    assert!(r["result"].is_object());
}

#[test]
fn handler_panic_becomes_internal_error() {
    let server = Server::with_tools(vec![Box::new(Panics)]);
    let mut conn = Conn::default();
    init(&server, &mut conn);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = server
        .handle_bytes(
            &mut conn,
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"boom"}}"#,
        )
        .unwrap();
    std::panic::set_hook(prev);
    assert_eq!(r["error"]["code"], -32603);
    assert_eq!(r["id"], 1);
    // The connection keeps working afterwards.
    let r = server
        .handle_bytes(&mut conn, br#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
        .unwrap();
    assert_eq!(r["result"], json!({}));
}

#[test]
fn notifications_initialized_marks_ready() {
    let server = Server::new();
    let mut conn = Conn::default();
    init(&server, &mut conn);
    assert!(server
        .handle_bytes(
            &mut conn,
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
    assert!(matches!(
        conn.legacy,
        mcp_server::server::LegacyState::Initialized { ready: true, .. }
    ));
}

#[test]
fn http_header_value_decoding() {
    use mcp_server::transport::http::decode_header_value;
    assert_eq!(decode_header_value("plain").as_deref(), Some("plain"));
    assert_eq!(
        decode_header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
        Some("Hello, 世界")
    );
    assert_eq!(decode_header_value("=?base64?***?="), None);
}
