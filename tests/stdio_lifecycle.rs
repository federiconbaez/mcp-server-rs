//! MCP lifecycle and server features over stdio, both protocol eras.

mod common;
use common::*;
use serde_json::{json, Value};

fn init_params(v: &str) -> Value {
    json!({"protocolVersion": v, "capabilities": {}, "clientInfo": {"name":"t","version":"0"}})
}

#[test]
fn requests_before_initialize_are_rejected_but_ping_works() {
    let mut c = StdioClient::spawn();
    assert_eq!(err_code(&c.call(req(1, "tools/list", json!({})))), -32600);
    assert_eq!(c.call(req(2, "ping", json!({})))["result"], json!({}));
}

#[test]
fn version_negotiation() {
    for v in ["2025-11-25", "2025-06-18", "2025-03-26"] {
        let mut c = StdioClient::spawn();
        let r = c.call(req(1, "initialize", init_params(v)));
        assert_eq!(r["result"]["protocolVersion"], v);
        assert_eq!(r["result"]["serverInfo"]["name"], "mcp-server-rs");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }
    let mut c = StdioClient::spawn();
    let r = c.call(req(1, "initialize", init_params("1999-01-01")));
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn initialize_validation_and_double_initialize() {
    let mut c = StdioClient::spawn();
    assert_eq!(
        err_code(&c.call(req(1, "initialize", json!({"capabilities":{}})))),
        -32602
    );
    assert_eq!(
        err_code(&c.call(json!({"jsonrpc":"2.0","id":2,"method":"initialize"}))),
        -32602
    );
    assert!(c.call(req(3, "initialize", init_params("2025-11-25")))["result"].is_object());
    assert_eq!(
        err_code(&c.call(req(4, "initialize", init_params("2025-11-25")))),
        -32600
    );
}

#[test]
fn tools_list_is_deterministic_and_well_formed() {
    let mut c = StdioClient::initialized("2025-11-25");
    let a = c.call(req(1, "tools/list", json!({})));
    let b = c.call(req(2, "tools/list", json!({})));
    assert_eq!(a["result"], b["result"]);
    let tools = a["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["calc", "echo"]);
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(t["description"].is_string());
    }
}

#[test]
fn tools_call_echo_and_errors() {
    let mut c = StdioClient::initialized("2025-11-25");
    let r = c.call(req(
        1,
        "tools/call",
        json!({"name":"echo","arguments":{"text":"hola"}}),
    ));
    assert_eq!(
        r["result"]["content"][0],
        json!({"type":"text","text":"hola"})
    );
    assert_eq!(r["result"]["isError"], false);
    // Unknown tool: protocol error.
    assert_eq!(
        err_code(&c.call(req(2, "tools/call", json!({"name":"nope"})))),
        -32602
    );
    assert_eq!(err_code(&c.call(req(3, "tools/call", json!({})))), -32602);
    // Bad arguments: tool execution error.
    let r = c.call(req(
        4,
        "tools/call",
        json!({"name":"echo","arguments":{"text":5}}),
    ));
    assert_eq!(r["result"]["isError"], true);
    let r = c.call(req(5, "tools/call", json!({"name":"echo","arguments":"x"})));
    assert_eq!(r["result"]["isError"], true);
}

#[test]
fn calc_runs_in_sandbox() {
    let mut c = StdioClient::initialized("2025-11-25");
    let r = c.call(req(
        1,
        "tools/call",
        json!({"name":"calc","arguments":{"expression":"(1 + 2) * 7 - 2^3"}}),
    ));
    assert_eq!(r["result"]["structuredContent"]["value"], 13.0);
    assert_eq!(r["result"]["content"][0]["text"], "13");
    let r = c.call(req(
        2,
        "tools/call",
        json!({"name":"calc","arguments":{"expression":"1/0"}}),
    ));
    assert_eq!(r["result"]["isError"], true);
    let deep = "(".repeat(200) + "1" + &")".repeat(200);
    let r = c.call(req(
        3,
        "tools/call",
        json!({"name":"calc","arguments":{"expression":deep}}),
    ));
    assert_eq!(r["result"]["isError"], true);
}

#[test]
fn resources_and_prompts_legacy() {
    let mut c = StdioClient::initialized("2025-11-25");
    let r = c.call(req(1, "resources/list", json!({})));
    assert_eq!(r["result"]["resources"].as_array().unwrap().len(), 2);
    let r = c.call(req(
        2,
        "resources/read",
        json!({"uri":"mcp-server-rs://versions"}),
    ));
    let text = r["result"]["contents"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap()["modern"][0],
        MODERN
    );
    assert_eq!(
        err_code(&c.call(req(
            3,
            "resources/read",
            json!({"uri":"file:///etc/passwd"})
        ))),
        -32002
    );
    assert_eq!(
        c.call(req(4, "resources/templates/list", json!({})))["result"]["resourceTemplates"],
        json!([])
    );

    let r = c.call(req(5, "prompts/list", json!({})));
    assert_eq!(r["result"]["prompts"][0]["name"], "review_code");
    let r = c.call(req(
        6,
        "prompts/get",
        json!({"name":"review_code","arguments":{"code":"fn main(){}","language":"rust"}}),
    ));
    assert_eq!(r["result"]["messages"][0]["role"], "user");
    assert!(r["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("fn main(){}"));
    assert_eq!(
        err_code(&c.call(req(7, "prompts/get", json!({"name":"review_code"})))),
        -32602
    );
    assert_eq!(
        err_code(&c.call(req(8, "prompts/get", json!({"name":"x"})))),
        -32602
    );
}

#[test]
fn unknown_cursor_is_invalid_params() {
    let mut c = StdioClient::initialized("2025-11-25");
    assert_eq!(
        err_code(&c.call(req(1, "tools/list", json!({"cursor":"abc"})))),
        -32602
    );
}

#[test]
fn exits_cleanly_on_eof() {
    let c = StdioClient::initialized("2025-11-25");
    assert!(c.close());
}

#[test]
fn modern_discover_without_handshake() {
    let mut c = StdioClient::spawn();
    let r = c.call(req(1, "server/discover", modern_params(json!({}))));
    let res = &r["result"];
    assert_eq!(res["supportedVersions"], json!([MODERN]));
    assert_eq!(res["resultType"], "complete");
    assert_eq!(
        res["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "mcp-server-rs"
    );
    assert!(res["ttlMs"].is_u64());
    assert_eq!(res["cacheScope"], "public");
}

#[test]
fn modern_unsupported_version() {
    let mut c = StdioClient::spawn();
    let r = c.call(req(1, "tools/list", json!({"_meta":{PV_KEY:"1900-01-01"}})));
    assert_eq!(err_code(&r), -32022);
    assert_eq!(r["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(r["error"]["data"]["supported"], json!([MODERN]));
    let r = c.call(req(2, "tools/list", json!({"_meta":{PV_KEY:5}})));
    assert_eq!(err_code(&r), -32602);
}

#[test]
fn modern_semantics() {
    let mut c = StdioClient::spawn();
    let r = c.call(req(1, "tools/list", modern_params(json!({}))));
    assert_eq!(r["result"]["resultType"], "complete");
    assert_eq!(r["result"]["ttlMs"], 60000);
    let r = c.call(req(
        2,
        "tools/call",
        modern_params(json!({"name":"echo","arguments":{"text":"x"}})),
    ));
    assert_eq!(r["result"]["resultType"], "complete");
    assert!(r["result"].get("ttlMs").is_none());
    // ping was removed in 2026-07-28.
    assert_eq!(
        err_code(&c.call(req(3, "ping", modern_params(json!({}))))),
        -32601
    );
    // resource not found is -32602 in 2026-07-28.
    let r = c.call(req(
        4,
        "resources/read",
        modern_params(json!({"uri":"x://nope"})),
    ));
    assert_eq!(err_code(&r), -32602);
}

#[test]
fn legacy_session_does_not_expose_discover() {
    let mut c = StdioClient::initialized("2025-11-25");
    assert_eq!(
        err_code(&c.call(req(1, "server/discover", json!({})))),
        -32601
    );
    // Legacy results carry no modern decoration.
    assert!(c.call(req(2, "tools/list", json!({})))["result"]
        .get("resultType")
        .is_none());
}
