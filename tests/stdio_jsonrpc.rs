//! JSON-RPC 2.0 conformance over the stdio transport.

mod common;
use common::*;
use serde_json::{json, Value};

#[test]
fn parse_error_has_null_id() {
    let mut c = StdioClient::spawn();
    let r = c.raw_call(r#"{"jsonrpc":"2.0","id":1,"method":"ping""#);
    assert_eq!(err_code(&r), -32700);
    assert_eq!(r["id"], Value::Null);
    assert_eq!(r["jsonrpc"], "2.0");
}

#[test]
fn invalid_utf8_is_parse_error() {
    let mut c = StdioClient::spawn();
    c.send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\"}");
    assert_eq!(err_code(&c.recv()), -32700);
}

#[test]
fn non_object_messages_are_invalid_requests() {
    let mut c = StdioClient::spawn();
    for raw in ["1", "\"ping\"", "true", "null"] {
        let r = c.raw_call(raw);
        assert_eq!(err_code(&r), -32600, "{raw}");
        assert_eq!(r["id"], Value::Null);
    }
}

#[test]
fn wrong_or_missing_jsonrpc_version_keeps_id() {
    let mut c = StdioClient::spawn();
    let r = c.call(json!({"jsonrpc":"1.0","id":"abc","method":"ping"}));
    assert_eq!((err_code(&r), r["id"].clone()), (-32600, json!("abc")));
    let r = c.call(json!({"id":7,"method":"ping"}));
    assert_eq!((err_code(&r), r["id"].clone()), (-32600, json!(7)));
}

#[test]
fn malformed_members_are_invalid_requests() {
    let mut c = StdioClient::spawn();
    for bad in [
        json!({"jsonrpc":"2.0","id":1}),
        json!({"jsonrpc":"2.0","id":1,"method":42}),
        json!({"jsonrpc":"2.0","id":1,"method":"ping","params":"x"}),
        json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":1.5,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":{"a":1},"method":"ping"}),
        json!({"jsonrpc":"2.0","id":1,"method":"ping","result":{}}),
    ] {
        let r = c.call(bad.clone());
        assert_eq!(err_code(&r), -32600, "{bad}");
    }
}

#[test]
fn string_and_integer_ids_are_echoed() {
    let mut c = StdioClient::spawn();
    assert_eq!(
        c.call(json!({"jsonrpc":"2.0","id":"x-1","method":"ping"}))["id"],
        "x-1"
    );
    assert_eq!(
        c.call(json!({"jsonrpc":"2.0","id":-5,"method":"ping"}))["id"],
        -5
    );
    let r = c.call(json!({"jsonrpc":"2.0","id":9,"method":"ping"}));
    assert_eq!(r["result"], json!({}));
}

#[test]
fn unknown_method_and_array_params() {
    let mut c = StdioClient::initialized("2025-11-25");
    assert_eq!(
        err_code(&c.call(req(1, "no/such/method", json!({})))),
        -32601
    );
    let r = c.call(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":[1,2]}));
    assert_eq!(err_code(&r), -32602);
}

#[test]
fn notifications_and_responses_get_no_reply() {
    let mut c = StdioClient::initialized("2025-11-25");
    c.send_value(&json!({"jsonrpc":"2.0","method":"notifications/unknown"}));
    c.send_value(
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}),
    );
    c.send_value(&json!({"jsonrpc":"2.0","id":5,"result":{}}));
    c.send_value(&json!({"jsonrpc":"2.0","id":6,"error":{"code":1,"message":"x"}}));
    // The next line on stdout must be the reply to this ping.
    let r = c.call(req(42, "ping", json!({})));
    assert_eq!(r["id"], 42);
    c.expect_silence();
}

#[test]
fn blank_lines_are_ignored() {
    let mut c = StdioClient::spawn();
    c.send_raw(b"");
    c.send_raw(b"   \t");
    assert_eq!(c.call(req(1, "ping", json!({})))["id"], 1);
}

#[test]
fn empty_batch_is_single_invalid_request() {
    let mut c = StdioClient::initialized("2025-03-26");
    let r = c.raw_call("[]");
    assert!(r.is_object());
    assert_eq!(err_code(&r), -32600);
}

#[test]
fn batch_rejected_after_2025_06_18() {
    for v in ["2025-06-18", "2025-11-25"] {
        let mut c = StdioClient::initialized(v);
        let r = c.call(json!([req(1, "ping", json!({}))]));
        assert_eq!(err_code(&r), -32600, "{v}");
    }
}

#[test]
fn batch_rejected_before_initialize() {
    let mut c = StdioClient::spawn();
    assert_eq!(
        err_code(&c.call(json!([req(1, "ping", json!({}))]))),
        -32600
    );
}

#[test]
fn batch_with_2025_03_26_mixed_members() {
    let mut c = StdioClient::initialized("2025-03-26");
    let r = c.call(json!([
        req(1, "ping", json!({})),
        {"jsonrpc":"2.0","method":"notifications/initialized"},
        1,
        req(2, "nope", json!({})),
        {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}
    ]));
    let arr = r.as_array().expect("batch reply is an array");
    assert_eq!(arr.len(), 4, "notification must not produce a reply: {r}");
    let find = |id: Value| arr.iter().find(|m| m["id"] == id).cloned().unwrap();
    assert_eq!(find(json!(1))["result"], json!({}));
    assert_eq!(err_code(&find(json!(2))), -32601);
    assert_eq!(find(json!(3))["result"]["content"][0]["text"], "hi");
    assert!(arr
        .iter()
        .any(|m| m["id"].is_null() && err_code(m) == -32600));
}

#[test]
fn batch_of_only_notifications_is_silent() {
    let mut c = StdioClient::initialized("2025-03-26");
    c.send_value(&json!([
        {"jsonrpc":"2.0","method":"notifications/a"},
        {"jsonrpc":"2.0","method":"notifications/b"}
    ]));
    c.expect_silence();
}

#[test]
fn initialize_inside_batch_is_rejected() {
    let mut c = StdioClient::initialized("2025-03-26");
    let r = c.call(json!([req(
        9,
        "initialize",
        json!({"protocolVersion":"2025-03-26","capabilities":{}})
    )]));
    assert_eq!(err_code(&r[0]), -32600);
    assert_eq!(r[0]["id"], 9);
}
