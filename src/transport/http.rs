//! Streamable HTTP transport (single `/mcp` endpoint).
//!
//! * Legacy clients (2025-03-26 .. 2025-11-25) get a session: `initialize`
//!   mints an `Mcp-Session-Id`, later requests must send it, `DELETE`
//!   terminates it. The optional standalone `GET` SSE stream is not offered
//!   (`405`), which the spec allows.
//! * Modern clients (2026-07-28) are served statelessly; the
//!   `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers are
//!   validated against the body (`-32020 HeaderMismatch`).
//!
//! Responses are always `application/json`; this server never needs an SSE
//! response stream because it emits no request-scoped notifications.

use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::jsonrpc::{self, Incoming, RpcError};
use crate::protocol::*;
use crate::server::{modern_version, Conn, Server};

pub const ENDPOINT: &str = "/mcp";

/// Transport configuration.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Extra allowed `Origin` values (exact match). Loopback origins are
    /// always allowed.
    pub allowed_origins: Vec<String>,
    pub max_body: usize,
    pub max_sessions: usize,
    pub workers: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            max_body: 4 * 1024 * 1024,
            max_sessions: 1024,
            workers: 4,
        }
    }
}

/// A transport-agnostic HTTP response.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
}

impl HttpResponse {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(body),
        }
    }
    fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: None,
        }
    }
    fn error(status: u16, id: Value, err: RpcError) -> Self {
        Self::json(status, jsonrpc::error_response(id, &err))
    }
    fn with_header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

/// Shared state: the MCP server plus legacy sessions.
pub struct HttpState {
    server: Server,
    cfg: HttpConfig,
    sessions: Mutex<HashMap<String, Arc<Mutex<Conn>>>>,
}

/// Case-insensitive header lookup over a list of (name, value) pairs.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn new_session_id() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("OS randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn origin_allowed(origin: &str, extra: &[String]) -> bool {
    if extra.iter().any(|o| o == origin) {
        return true;
    }
    let rest = match origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    {
        Some(r) => r,
        None => return false,
    };
    let host = if let Some(v6) = rest.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        rest.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Decode an `Mcp-Name` / `Mcp-Param-*` value (Base64 sentinel aware).
pub fn decode_header_value(v: &str) -> Option<String> {
    match v
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        None => Some(v.to_string()),
    }
}

fn mismatch(id: Value, msg: impl Into<String>) -> HttpResponse {
    HttpResponse::error(
        400,
        id,
        RpcError::new(
            jsonrpc::HEADER_MISMATCH,
            format!("Header mismatch: {}", msg.into()),
        ),
    )
}

impl HttpState {
    pub fn new(server: Server, cfg: HttpConfig) -> Self {
        Self {
            server,
            cfg,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    /// Handle one HTTP request. Pure function of its inputs plus session
    /// state, so it can be tested without sockets.
    pub fn handle(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> HttpResponse {
        let path = path.split('?').next().unwrap_or(path);
        if path != ENDPOINT {
            return HttpResponse::empty(404);
        }
        if let Some(origin) = header(headers, "origin") {
            if !origin_allowed(origin, &self.cfg.allowed_origins) {
                return HttpResponse::error(
                    403,
                    Value::Null,
                    RpcError::invalid_request("Origin not allowed"),
                );
            }
        }
        match method {
            "POST" => self.post(headers, body),
            "DELETE" => self.delete(headers),
            _ => HttpResponse::empty(405).with_header("Allow", "POST, DELETE"),
        }
    }

    fn delete(&self, headers: &[(String, String)]) -> HttpResponse {
        match header(headers, "mcp-session-id") {
            None => HttpResponse::empty(405).with_header("Allow", "POST, DELETE"),
            Some(id) => match self.sessions.lock().unwrap().remove(id) {
                Some(_) => HttpResponse::empty(200),
                None => HttpResponse::empty(404),
            },
        }
    }

    fn post(&self, headers: &[(String, String)], body: &[u8]) -> HttpResponse {
        let ct = header(headers, "content-type").unwrap_or("");
        if !ct
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("application/json")
        {
            return HttpResponse::empty(415);
        }
        if let Some(accept) = header(headers, "accept") {
            let a = accept.to_ascii_lowercase();
            if !(a.contains("application/json") || a.contains("*/*") || a.contains("application/*"))
            {
                return HttpResponse::empty(406);
            }
        }
        if body.len() > self.cfg.max_body {
            return HttpResponse::empty(413);
        }
        let value: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return HttpResponse::error(400, Value::Null, RpcError::parse_error()),
        };

        let is_modern_body = value
            .get("params")
            .and_then(|p| p.get("_meta"))
            .and_then(|m| m.get(META_PROTOCOL_VERSION))
            .is_some();
        let hdr_version = header(headers, "mcp-protocol-version");

        // Legacy session traffic.
        if let Some(sid) = header(headers, "mcp-session-id") {
            let conn = self.sessions.lock().unwrap().get(sid).cloned();
            match conn {
                Some(conn) => return self.legacy_in_session(&conn, hdr_version, value),
                // Modern requests ignore a stray session header.
                None if is_modern_body => {}
                None => {
                    return HttpResponse::error(
                        404,
                        jsonrpc::salvage_id(&value),
                        RpcError::invalid_request("unknown or terminated session"),
                    )
                }
            }
        }

        if value.is_array() {
            return HttpResponse::error(
                400,
                Value::Null,
                RpcError::invalid_request("batches require an initialized 2025-03-26 session"),
            );
        }
        let msg = match jsonrpc::validate(&value) {
            Ok(m) => m,
            Err((id, e)) => return HttpResponse::error(400, id, e),
        };
        if msg == Incoming::Response {
            return HttpResponse::empty(202);
        }
        if msg.method() == Some("initialize") {
            return self.initialize(msg);
        }
        if is_modern_body || hdr_version == Some(MODERN_VERSION) {
            return self.modern(headers, &value, msg);
        }
        HttpResponse::error(
            400,
            jsonrpc::salvage_id(&value),
            RpcError::invalid_request("missing Mcp-Session-Id (send initialize first)"),
        )
    }

    fn initialize(&self, msg: Incoming) -> HttpResponse {
        if self.session_count() >= self.cfg.max_sessions {
            return HttpResponse::empty(503);
        }
        let mut conn = Conn::default();
        let reply = self.server.handle_message(&mut conn, msg);
        let reply = match reply {
            Some(r) => r,
            None => return HttpResponse::empty(202), // initialize sent as notification
        };
        if reply.get("result").is_none() {
            return HttpResponse::json(400, reply);
        }
        let sid = new_session_id();
        self.sessions
            .lock()
            .unwrap()
            .insert(sid.clone(), Arc::new(Mutex::new(conn)));
        HttpResponse::json(200, reply).with_header("Mcp-Session-Id", &sid)
    }

    fn legacy_in_session(
        &self,
        conn: &Mutex<Conn>,
        hdr_version: Option<&str>,
        value: Value,
    ) -> HttpResponse {
        let mut conn = conn.lock().unwrap();
        let negotiated = conn
            .negotiated_version()
            .unwrap_or(HTTP_DEFAULT_LEGACY_VERSION);
        if let Some(h) = hdr_version {
            if h != negotiated {
                return HttpResponse::error(
                    400,
                    jsonrpc::salvage_id(&value),
                    RpcError::invalid_request(format!(
                        "MCP-Protocol-Version '{h}' does not match negotiated '{negotiated}'"
                    )),
                );
            }
        }
        match self.server.handle_value(&mut conn, value) {
            None => HttpResponse::empty(202),
            Some(reply) => {
                // A batch reply is an array (per-item errors inside); a
                // whole-message protocol error is a single object.
                let status = if is_protocol_error(&reply) { 400 } else { 200 };
                HttpResponse::json(status, reply)
            }
        }
    }

    fn modern(&self, headers: &[(String, String)], value: &Value, msg: Incoming) -> HttpResponse {
        let id = jsonrpc::salvage_id(value);
        let method = msg.method().unwrap_or_default().to_string();
        let params = msg.params();

        // Header ↔ body validation (2026-07-28 "Server Validation").
        let hdr_version = match header(headers, "mcp-protocol-version") {
            Some(v) => v,
            None => return mismatch(id, "missing MCP-Protocol-Version header"),
        };
        let body_version = params
            .and_then(|p| p.get("_meta"))
            .and_then(|m| m.get(META_PROTOCOL_VERSION))
            .and_then(Value::as_str);
        if body_version != Some(hdr_version) {
            return mismatch(
                id,
                format!(
                    "MCP-Protocol-Version '{hdr_version}' does not match body {body_version:?}"
                ),
            );
        }
        match header(headers, "mcp-method") {
            Some(m) if m == method => {}
            Some(m) => {
                return mismatch(
                    id,
                    format!("Mcp-Method '{m}' does not match body '{method}'"),
                )
            }
            None => return mismatch(id, "missing Mcp-Method header"),
        }
        let name_field = match method.as_str() {
            "tools/call" | "prompts/get" => Some("name"),
            "resources/read" => Some("uri"),
            _ => None,
        };
        if let Some(field) = name_field {
            let body_name = params.and_then(|p| p.get(field)).and_then(Value::as_str);
            let hdr_name = match header(headers, "mcp-name").map(decode_header_value) {
                None => return mismatch(id, "missing Mcp-Name header"),
                Some(None) => return mismatch(id, "Mcp-Name is not valid base64/UTF-8"),
                Some(Some(n)) => n,
            };
            if body_name != Some(hdr_name.as_str()) {
                return mismatch(
                    id,
                    format!("Mcp-Name '{hdr_name}' does not match body {body_name:?}"),
                );
            }
        }
        if let Some(Err(e)) = modern_version(params) {
            return HttpResponse::error(400, id, e);
        }

        let mut conn = Conn::default();
        match self.server.handle_message(&mut conn, msg) {
            None => HttpResponse::empty(202),
            Some(reply) => {
                let code = reply.pointer("/error/code").and_then(Value::as_i64);
                let status = match code {
                    Some(jsonrpc::METHOD_NOT_FOUND) => 404,
                    Some(
                        jsonrpc::UNSUPPORTED_PROTOCOL_VERSION
                        | jsonrpc::HEADER_MISMATCH
                        | jsonrpc::MISSING_REQUIRED_CLIENT_CAPABILITY
                        | jsonrpc::INVALID_REQUEST
                        | jsonrpc::PARSE_ERROR,
                    ) => 400,
                    _ => 200,
                };
                HttpResponse::json(status, reply)
            }
        }
    }
}

fn is_protocol_error(reply: &Value) -> bool {
    matches!(
        reply.pointer("/error/code").and_then(Value::as_i64),
        Some(jsonrpc::PARSE_ERROR | jsonrpc::INVALID_REQUEST)
    )
}

/// A running HTTP server.
pub struct HttpServer {
    http: Arc<tiny_http::Server>,
    state: Arc<HttpState>,
    addr: SocketAddr,
}

impl HttpServer {
    pub fn bind(addr: &str, server: Server, cfg: HttpConfig) -> std::io::Result<Self> {
        let http = tiny_http::Server::http(addr).map_err(std::io::Error::other)?;
        let addr = http
            .server_addr()
            .to_ip()
            .ok_or_else(|| std::io::Error::other("not an IP listener"))?;
        Ok(Self {
            http: Arc::new(http),
            state: Arc::new(HttpState::new(server, cfg)),
            addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn state(&self) -> Arc<HttpState> {
        self.state.clone()
    }

    /// Start worker threads and return immediately.
    pub fn spawn(&self) -> Vec<std::thread::JoinHandle<()>> {
        (0..self.state.cfg.workers.max(1))
            .map(|_| {
                let http = self.http.clone();
                let state = self.state.clone();
                std::thread::spawn(move || {
                    while let Ok(req) = http.recv() {
                        serve_one(&state, req);
                    }
                })
            })
            .collect()
    }

    /// Serve until [`HttpServer::shutdown`] is called from another thread.
    pub fn run(&self) {
        for h in self.spawn() {
            let _ = h.join();
        }
    }

    /// Wake and stop all worker threads.
    pub fn shutdown(&self) {
        for _ in 0..self.state.cfg.workers.max(1) {
            self.http.unblock();
        }
    }
}

fn serve_one(state: &HttpState, mut req: tiny_http::Request) {
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|h| {
            (
                h.field.as_str().as_str().to_string(),
                h.value.as_str().to_string(),
            )
        })
        .collect();
    let method = req.method().as_str().to_string();
    let url = req.url().to_string();
    let mut body = Vec::new();
    let limit = state.cfg.max_body as u64 + 1;
    let resp = match req.as_reader().take(limit).read_to_end(&mut body) {
        Ok(_) => state.handle(&method, &url, &headers, &body),
        Err(_) => HttpResponse::error(400, Value::Null, RpcError::parse_error()),
    };
    let data = resp
        .body
        .as_ref()
        .map(|b| b.to_string().into_bytes())
        .unwrap_or_default();
    let mut out = tiny_http::Response::from_data(data).with_status_code(resp.status);
    for (k, v) in &resp.headers {
        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            out.add_header(h);
        }
    }
    let _ = req.respond(out);
}
