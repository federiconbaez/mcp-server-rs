//! Streamable HTTP transport conformance (real sockets, in-process server).

mod common;
use common::*;
use mcp_server::transport::http::{HttpConfig, HttpServer};
use mcp_server::Server;
use serde_json::{json, Value};
use std::net::SocketAddr;

fn start(cfg: HttpConfig) -> SocketAddr {
    let srv = HttpServer::bind("127.0.0.1:0", Server::new().worker_exe(BIN), cfg).unwrap();
    let addr = srv.local_addr();
    srv.spawn();
    // Keep the listener alive for the rest of the test process.
    std::mem::forget(srv);
    addr
}

fn server() -> SocketAddr {
    start(HttpConfig::default())
}

fn post(addr: SocketAddr, extra: &[(&str, &str)], body: &Value) -> Resp {
    let mut h: Vec<(&str, &str)> = JSON_HEADERS.to_vec();
    h.extend_from_slice(extra);
    http(addr, "POST", "/mcp", &h, &body.to_string())
}

/// Legacy handshake over HTTP; returns the session id.
fn open_session(addr: SocketAddr, version: &str) -> String {
    let r = post(
        addr,
        &[],
        &req(
            1,
            "initialize",
            json!({
        "protocolVersion": version, "capabilities": {}, "clientInfo": {"name":"t","version":"0"}}),
        ),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.headers["content-type"], "application/json");
    let sid = r.headers.get("mcp-session-id").expect("session id").clone();
    assert!(sid.len() >= 32 && sid.bytes().all(|b| (0x21..=0x7e).contains(&b)));
    let n = post(
        addr,
        &[("Mcp-Session-Id", &sid), ("MCP-Protocol-Version", version)],
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    assert_eq!(n.status, 202);
    assert!(n.body.is_empty());
    sid
}

#[test]
fn legacy_session_flow() {
    let a = server();
    let sid = open_session(a, "2025-11-25");
    let h = [
        ("Mcp-Session-Id", sid.as_str()),
        ("MCP-Protocol-Version", "2025-11-25"),
    ];
    let r = post(a, &h, &req(2, "tools/list", json!({})));
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["result"]["tools"][1]["name"], "echo");
    let r = post(
        a,
        &h,
        &req(
            3,
            "tools/call",
            json!({"name":"calc","arguments":{"expression":"6*7"}}),
        ),
    );
    assert_eq!(r.json()["result"]["structuredContent"]["value"], 42.0);
    // Client responses are accepted with 202.
    assert_eq!(
        post(a, &h, &json!({"jsonrpc":"2.0","id":"s1","result":{}})).status,
        202
    );
}

#[test]
fn missing_and_unknown_session() {
    let a = server();
    let r = post(a, &[], &req(1, "tools/list", json!({})));
    assert_eq!(r.status, 400);
    assert_eq!(err_code(&r.json()), -32600);
    let r = post(
        a,
        &[("Mcp-Session-Id", "deadbeef")],
        &req(1, "tools/list", json!({})),
    );
    assert_eq!(r.status, 404);
}

#[test]
fn delete_terminates_session() {
    let a = server();
    let sid = open_session(a, "2025-06-18");
    let r = http(a, "DELETE", "/mcp", &[("Mcp-Session-Id", &sid)], "");
    assert_eq!(r.status, 200);
    let r = post(a, &[("Mcp-Session-Id", &sid)], &req(2, "ping", json!({})));
    assert_eq!(r.status, 404);
    assert_eq!(
        http(a, "DELETE", "/mcp", &[("Mcp-Session-Id", &sid)], "").status,
        404
    );
    assert_eq!(http(a, "DELETE", "/mcp", &[], "").status, 405);
}

#[test]
fn get_and_other_paths() {
    let a = server();
    let r = http(a, "GET", "/mcp", &[("Accept", "text/event-stream")], "");
    assert_eq!(r.status, 405);
    assert!(r.headers["allow"].contains("POST"));
    assert_eq!(http(a, "PUT", "/mcp", &[], "").status, 405);
    assert_eq!(post_path(a, "/other").status, 404);
}

fn post_path(a: SocketAddr, path: &str) -> Resp {
    http(
        a,
        "POST",
        path,
        JSON_HEADERS,
        &req(1, "ping", json!({})).to_string(),
    )
}

#[test]
fn origin_validation() {
    let a = start(HttpConfig {
        allowed_origins: vec!["https://app.example".into()],
        ..Default::default()
    });
    let init = req(
        1,
        "initialize",
        json!({"protocolVersion":"2025-11-25","capabilities":{}}),
    );
    let r = post(a, &[("Origin", "https://evil.example")], &init);
    assert_eq!(r.status, 403);
    assert_eq!(post(a, &[("Origin", "null")], &init).status, 403);
    assert_eq!(
        post(a, &[("Origin", "http://localhost.evil.example")], &init).status,
        403
    );
    assert_eq!(
        post(a, &[("Origin", "http://localhost:3000")], &init).status,
        200
    );
    assert_eq!(post(a, &[("Origin", "http://[::1]:9")], &init).status, 200);
    assert_eq!(
        post(a, &[("Origin", "https://app.example")], &init).status,
        200
    );
}

#[test]
fn content_negotiation_and_limits() {
    let a = start(HttpConfig {
        max_body: 256,
        ..Default::default()
    });
    let body = req(1, "ping", json!({})).to_string();
    let r = http(a, "POST", "/mcp", &[("Content-Type", "text/plain")], &body);
    assert_eq!(r.status, 415);
    let r = http(
        a,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", "text/html"),
        ],
        &body,
    );
    assert_eq!(r.status, 406);
    let big = json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{"pad":"x".repeat(1000)}});
    assert_eq!(post(a, &[], &big).status, 413);
    let r = http(a, "POST", "/mcp", JSON_HEADERS, "{nope");
    assert_eq!(r.status, 400);
    assert_eq!(err_code(&r.json()), -32700);
    let r = http(
        a,
        "POST",
        "/mcp",
        JSON_HEADERS,
        r#"{"jsonrpc":"2.0","id":1}"#,
    );
    assert_eq!(r.status, 400);
    assert_eq!(err_code(&r.json()), -32600);
}

#[test]
fn legacy_protocol_version_header_must_match() {
    let a = server();
    let sid = open_session(a, "2025-11-25");
    let r = post(
        a,
        &[
            ("Mcp-Session-Id", &sid),
            ("MCP-Protocol-Version", "2025-03-26"),
        ],
        &req(2, "ping", json!({})),
    );
    assert_eq!(r.status, 400);
    // Header omitted: accepted (spec fallback).
    let r = post(a, &[("Mcp-Session-Id", &sid)], &req(3, "ping", json!({})));
    assert_eq!(r.status, 200);
}

#[test]
fn batches_only_in_2025_03_26_sessions() {
    let a = server();
    let sid = open_session(a, "2025-03-26");
    let r = post(
        a,
        &[("Mcp-Session-Id", &sid)],
        &json!([req(2, "ping", json!({})), req(3, "tools/list", json!({}))]),
    );
    assert_eq!(r.status, 200);
    assert_eq!(r.json().as_array().unwrap().len(), 2);
    let r = post(
        a,
        &[("Mcp-Session-Id", &sid)],
        &json!([{"jsonrpc":"2.0","method":"notifications/x"}]),
    );
    assert_eq!(r.status, 202);

    let sid = open_session(a, "2025-11-25");
    let r = post(
        a,
        &[("Mcp-Session-Id", &sid)],
        &json!([req(2, "ping", json!({}))]),
    );
    assert_eq!(r.status, 400);
    // Batch without any session.
    assert_eq!(
        post(a, &[], &json!([req(2, "ping", json!({}))])).status,
        400
    );
}

fn modern_headers<'a>(method: &'a str, name: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut h = vec![("MCP-Protocol-Version", MODERN), ("Mcp-Method", method)];
    if let Some(n) = name {
        h.push(("Mcp-Name", n));
    }
    h
}

#[test]
fn modern_stateless_requests() {
    let a = server();
    let r = post(
        a,
        &modern_headers("server/discover", None),
        &req(1, "server/discover", modern_params(json!({}))),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.headers.contains_key("mcp-session-id"));
    assert_eq!(r.json()["result"]["supportedVersions"], json!([MODERN]));

    let body = req(
        2,
        "tools/call",
        modern_params(json!({"name":"echo","arguments":{"text":"ok"}})),
    );
    let r = post(a, &modern_headers("tools/call", Some("echo")), &body);
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["result"]["resultType"], "complete");
    // Base64 sentinel form of Mcp-Name ("echo").
    let r = post(
        a,
        &modern_headers("tools/call", Some("=?base64?ZWNobw==?=")),
        &body,
    );
    assert_eq!(r.status, 200);
    // A stray (unknown) session id is ignored for modern requests.
    let mut h = modern_headers("tools/call", Some("echo"));
    h.push(("Mcp-Session-Id", "stale"));
    assert_eq!(post(a, &h, &body).status, 200);
}

#[test]
fn modern_header_mismatches() {
    let a = server();
    let body = req(
        1,
        "tools/call",
        modern_params(json!({"name":"echo","arguments":{"text":"x"}})),
    );
    let cases: Vec<Vec<(&str, &str)>> = vec![
        vec![("Mcp-Method", "tools/call"), ("Mcp-Name", "echo")],
        vec![("MCP-Protocol-Version", MODERN), ("Mcp-Name", "echo")],
        vec![
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/list"),
            ("Mcp-Name", "echo"),
        ],
        vec![
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
        ],
        vec![
            ("MCP-Protocol-Version", MODERN),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "calc"),
        ],
        vec![
            ("MCP-Protocol-Version", "2025-11-25"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "echo"),
        ],
    ];
    for h in cases {
        let r = post(a, &h, &body);
        assert_eq!(r.status, 400, "{h:?}");
        assert_eq!(err_code(&r.json()), -32020, "{h:?}");
        assert_eq!(r.json()["id"], 1);
    }
    // Header says modern, body has no _meta.
    let r = post(
        a,
        &modern_headers("tools/list", None),
        &req(2, "tools/list", json!({})),
    );
    assert_eq!(err_code(&r.json()), -32020);
}

#[test]
fn modern_version_and_method_errors() {
    let a = server();
    let old = json!({"_meta": {PV_KEY: "1900-01-01"}});
    let h = vec![
        ("MCP-Protocol-Version", "1900-01-01"),
        ("Mcp-Method", "tools/list"),
    ];
    let r = post(a, &h, &req(1, "tools/list", old));
    assert_eq!(r.status, 400);
    assert_eq!(err_code(&r.json()), -32022);
    assert_eq!(r.json()["error"]["data"]["supported"], json!([MODERN]));

    let r = post(
        a,
        &modern_headers("nope/nope", None),
        &req(2, "nope/nope", modern_params(json!({}))),
    );
    assert_eq!(r.status, 404);
    assert_eq!(err_code(&r.json()), -32601);

    let n = json!({"jsonrpc":"2.0","method":"notifications/x","params":modern_params(json!({}))});
    assert_eq!(
        post(a, &modern_headers("notifications/x", None), &n).status,
        202
    );
}
