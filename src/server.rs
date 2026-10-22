//! Transport-independent MCP dispatcher.
//!
//! The server is *dual-era*:
//! * **legacy** revisions (`2025-11-25`, `2025-06-18`, `2025-03-26`) use the
//!   `initialize` / `notifications/initialized` handshake, scoped to a
//!   [`Conn`] (the stdio process, or one HTTP session);
//! * the **modern** revision (`2026-07-28`) is stateless: every request
//!   carries `_meta["io.modelcontextprotocol/protocolVersion"]`.

use serde_json::{json, Map, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use crate::jsonrpc::{self, Incoming, RpcError};
use crate::protocol::*;
use crate::tools::{self, Tool, ToolContext};

/// Which protocol era a request is served under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Era {
    Legacy(String),
    Modern,
}

/// Legacy handshake state of one connection / session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LegacyState {
    #[default]
    Fresh,
    Initialized {
        version: String,
        /// `notifications/initialized` received.
        ready: bool,
    },
}

/// Per-connection state (stdio process or HTTP session).
#[derive(Debug, Default)]
pub struct Conn {
    pub legacy: LegacyState,
}

impl Conn {
    pub fn negotiated_version(&self) -> Option<&str> {
        match &self.legacy {
            LegacyState::Initialized { version, .. } => Some(version),
            LegacyState::Fresh => None,
        }
    }
}

/// The MCP server: tool registry plus static resources and prompts.
pub struct Server {
    tools: Vec<Box<dyn Tool>>,
    ctx: ToolContext,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    /// Server with the built-in tools.
    pub fn new() -> Self {
        Self::with_tools(tools::builtin())
    }

    pub fn with_tools(tools: Vec<Box<dyn Tool>>) -> Self {
        Self {
            tools,
            ctx: ToolContext { worker_exe: None },
        }
    }

    /// Override the binary used for sandbox workers (default: current exe).
    pub fn worker_exe(mut self, path: impl Into<PathBuf>) -> Self {
        self.ctx.worker_exe = Some(path.into());
        self
    }

    /// Parse raw bytes and handle the resulting message or batch.
    /// Returns the serialized-ready response, or `None` if nothing must be sent.
    pub fn handle_bytes(&self, conn: &mut Conn, bytes: &[u8]) -> Option<Value> {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(v) => self.handle_value(conn, v),
            Err(_) => Some(jsonrpc::error_response(
                Value::Null,
                &RpcError::parse_error(),
            )),
        }
    }

    /// Whether a JSON-RPC batch is permitted on this connection.
    pub fn batch_allowed(conn: &Conn) -> bool {
        conn.negotiated_version() == Some(BATCH_VERSION)
    }

    /// Handle an already-parsed JSON value (single message or batch).
    pub fn handle_value(&self, conn: &mut Conn, v: Value) -> Option<Value> {
        let items = match v {
            Value::Array(items) => items,
            single => {
                return match jsonrpc::validate(&single) {
                    Ok(msg) => self.handle_message(conn, msg),
                    Err((id, e)) => Some(jsonrpc::error_response(id, &e)),
                }
            }
        };
        if items.is_empty() {
            return Some(jsonrpc::error_response(
                Value::Null,
                &RpcError::invalid_request("empty batch"),
            ));
        }
        if !Self::batch_allowed(conn) {
            return Some(jsonrpc::error_response(
                Value::Null,
                &RpcError::invalid_request(
                    "JSON-RPC batches are only supported after negotiating protocol version 2025-03-26",
                )
                .with_data(json!({ "negotiatedVersion": conn.negotiated_version() })),
            ));
        }
        let mut out = Vec::new();
        for item in items {
            match jsonrpc::validate(&item) {
                Ok(msg) if msg.method() == Some("initialize") => {
                    let id = jsonrpc::salvage_id(&item);
                    out.push(jsonrpc::error_response(
                        id,
                        &RpcError::invalid_request("initialize must not be part of a batch"),
                    ));
                }
                Ok(msg) => out.extend(self.handle_message(conn, msg)),
                Err((id, e)) => out.push(jsonrpc::error_response(id, &e)),
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(Value::Array(out))
        }
    }

    /// Decide the era a request will be served under, without side effects.
    pub fn classify(conn: &Conn, method: &str, params: Option<&Value>) -> Result<Era, RpcError> {
        if method == "initialize" {
            return Ok(Era::Legacy(String::new()));
        }
        if let Some(v) = conn.negotiated_version() {
            return Ok(Era::Legacy(v.to_string()));
        }
        match modern_version(params) {
            Some(Ok(_)) => Ok(Era::Modern),
            Some(Err(e)) => Err(e),
            None if method == "ping" => Ok(Era::Legacy(String::new())),
            None => Err(RpcError::invalid_request(
                "server not initialized: send `initialize` first, or include \
                 _meta[\"io.modelcontextprotocol/protocolVersion\"] (2026-07-28)",
            )),
        }
    }

    /// Handle one validated message.
    pub fn handle_message(&self, conn: &mut Conn, msg: Incoming) -> Option<Value> {
        match msg {
            Incoming::Response => None,
            Incoming::Notification { method, .. } => {
                if method == "notifications/initialized" {
                    if let LegacyState::Initialized { ready, .. } = &mut conn.legacy {
                        *ready = true;
                    }
                }
                // notifications/cancelled and unknown notifications: requests
                // are processed synchronously, so there is nothing to cancel.
                None
            }
            Incoming::Request { id, method, params } => {
                Some(match self.handle_request(conn, &method, params.as_ref()) {
                    Ok(result) => jsonrpc::response(&id, result),
                    Err(e) => jsonrpc::error_response(id.to_value(), &e),
                })
            }
        }
    }

    /// Handle a request and return its result or error.
    pub fn handle_request(
        &self,
        conn: &mut Conn,
        method: &str,
        params: Option<&Value>,
    ) -> Result<Value, RpcError> {
        if matches!(params, Some(Value::Array(_))) {
            return Err(RpcError::invalid_params("MCP params must be an object"));
        }
        let era = Self::classify(conn, method, params)?;
        if method == "initialize" {
            return self.initialize(conn, params);
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| self.route(&era, method, params)));
        let result = match outcome {
            Ok(r) => r?,
            Err(_) => return Err(RpcError::internal("handler panicked")),
        };
        Ok(match era {
            Era::Modern => decorate_modern(method, result),
            Era::Legacy(_) => result,
        })
    }

    fn initialize(&self, conn: &mut Conn, params: Option<&Value>) -> Result<Value, RpcError> {
        if conn.negotiated_version().is_some() {
            return Err(RpcError::invalid_request("already initialized"));
        }
        let p = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
        let requested = p
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("protocolVersion must be a string"))?;
        if !p.get("capabilities").is_some_and(Value::is_object) {
            return Err(RpcError::invalid_params("capabilities must be an object"));
        }
        // Spec: echo the requested version if supported, otherwise answer
        // with the latest version we support.
        let version = if is_legacy(requested) {
            requested
        } else {
            LEGACY_VERSIONS[0]
        };
        conn.legacy = LegacyState::Initialized {
            version: version.to_string(),
            ready: false,
        };
        Ok(json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false }
            },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
            "instructions": INSTRUCTIONS
        }))
    }

    fn route(&self, era: &Era, method: &str, params: Option<&Value>) -> Result<Value, RpcError> {
        let modern = *era == Era::Modern;
        match method {
            "ping" if !modern => Ok(json!({})),
            "server/discover" if modern => Ok(json!({
                "supportedVersions": [MODERN_VERSION],
                "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                "instructions": INSTRUCTIONS
            })),
            "tools/list" => {
                reject_cursor(params)?;
                let tools: Vec<Value> = self.tools.iter().map(|t| t.definition()).collect();
                Ok(json!({ "tools": tools }))
            }
            "tools/call" => self.call_tool(params),
            "resources/list" => {
                reject_cursor(params)?;
                Ok(json!({ "resources": resource_list() }))
            }
            "resources/templates/list" => {
                reject_cursor(params)?;
                Ok(json!({ "resourceTemplates": [] }))
            }
            "resources/read" => read_resource(params, modern),
            "prompts/list" => {
                reject_cursor(params)?;
                Ok(json!({ "prompts": [review_prompt_def()] }))
            }
            "prompts/get" => get_prompt(params),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    fn call_tool(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let p = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
        let name = p
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("name must be a string"))?;
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| RpcError::invalid_params(format!("unknown tool '{name}'")))?;
        let args = p.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let def = tool.definition();
        // Spec 2025-11-25+: input validation failures are tool execution
        // errors, so the model can see them and self-correct.
        if let Err(e) = tools::validate_schema(&def["inputSchema"], &args, "arguments") {
            return Ok(tool_error(format!("Invalid arguments: {e}")));
        }
        Ok(match tool.call(&args, &self.ctx) {
            Ok(v) => v,
            Err(e) => tool_error(e),
        })
    }
}

const INSTRUCTIONS: &str =
    "Demo MCP server. `echo` returns text; `calc` (Linux) evaluates arithmetic in a seccomp sandbox.";

/// `Some(Ok(version))` if params carry a supported modern version,
/// `Some(Err)` if they carry an invalid/unsupported one, `None` if absent.
pub fn modern_version(params: Option<&Value>) -> Option<Result<String, RpcError>> {
    let v = params?.get("_meta")?.get(META_PROTOCOL_VERSION)?;
    Some(match v.as_str() {
        None => Err(RpcError::invalid_params(format!(
            "_meta.{META_PROTOCOL_VERSION} must be a string"
        ))),
        Some(s) if s == MODERN_VERSION => Ok(s.to_string()),
        Some(s) => Err(unsupported_version(s)),
    })
}

pub fn unsupported_version(requested: &str) -> RpcError {
    RpcError::new(
        jsonrpc::UNSUPPORTED_PROTOCOL_VERSION,
        "Unsupported protocol version",
    )
    .with_data(json!({
        "supported": [MODERN_VERSION],
        "requested": requested,
        "legacyVersionsViaInitialize": LEGACY_VERSIONS
    }))
}

fn decorate_modern(method: &str, result: Value) -> Value {
    let mut obj = match result {
        Value::Object(o) => o,
        other => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            m
        }
    };
    obj.insert("resultType".into(), json!("complete"));
    let meta = obj.entry("_meta").or_insert_with(|| json!({}));
    if let Some(m) = meta.as_object_mut() {
        m.insert(
            META_SERVER_INFO.into(),
            json!({ "name": SERVER_NAME, "version": SERVER_VERSION }),
        );
    }
    let cacheable = matches!(
        method,
        "server/discover"
            | "tools/list"
            | "prompts/list"
            | "resources/list"
            | "resources/read"
            | "resources/templates/list"
    );
    if cacheable {
        obj.insert("ttlMs".into(), json!(LIST_TTL_MS));
        obj.insert("cacheScope".into(), json!("public"));
    }
    Value::Object(obj)
}

fn tool_error(msg: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": msg.into() }], "isError": true })
}

fn reject_cursor(params: Option<&Value>) -> Result<(), RpcError> {
    if params.and_then(|p| p.get("cursor")).is_some() {
        return Err(RpcError::invalid_params(
            "invalid cursor (all results fit in one page)",
        ));
    }
    Ok(())
}

const ABOUT_URI: &str = "mcp-server-rs://about";
const VERSIONS_URI: &str = "mcp-server-rs://versions";

fn resource_list() -> Value {
    json!([
        { "uri": ABOUT_URI, "name": "about", "title": "About this server", "mimeType": "text/markdown" },
        { "uri": VERSIONS_URI, "name": "versions", "title": "Supported protocol versions", "mimeType": "application/json" }
    ])
}

fn read_resource(params: Option<&Value>, modern: bool) -> Result<Value, RpcError> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("uri must be a string"))?;
    let (mime, text) = match uri {
        ABOUT_URI => (
            "text/markdown",
            format!("# {SERVER_NAME} {SERVER_VERSION}\n\n{INSTRUCTIONS}\n"),
        ),
        VERSIONS_URI => (
            "application/json",
            json!({ "modern": [MODERN_VERSION], "legacy": LEGACY_VERSIONS }).to_string(),
        ),
        _ => {
            // 2026-07-28 uses -32602; earlier revisions used -32002.
            let code = if modern {
                jsonrpc::INVALID_PARAMS
            } else {
                jsonrpc::LEGACY_RESOURCE_NOT_FOUND
            };
            return Err(RpcError::new(code, "Resource not found").with_data(json!({ "uri": uri })));
        }
    };
    Ok(json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] }))
}

fn review_prompt_def() -> Value {
    json!({
        "name": "review_code",
        "title": "Code review",
        "description": "Asks for a careful review of a code snippet.",
        "arguments": [
            { "name": "code", "description": "Code to review", "required": true },
            { "name": "language", "description": "Programming language", "required": false }
        ]
    })
}

fn get_prompt(params: Option<&Value>) -> Result<Value, RpcError> {
    let p = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let name = p
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("name must be a string"))?;
    if name != "review_code" {
        return Err(RpcError::invalid_params(format!("unknown prompt '{name}'")));
    }
    let args = match p.get("arguments") {
        None => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(RpcError::invalid_params("arguments must be an object")),
    };
    if args.values().any(|v| !v.is_string()) {
        return Err(RpcError::invalid_params("prompt arguments must be strings"));
    }
    let code = args
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing required argument 'code'"))?;
    let lang = args.get("language").and_then(Value::as_str).unwrap_or("");
    Ok(json!({
        "description": "Code review request",
        "messages": [{
            "role": "user",
            "content": {
                "type": "text",
                "text": format!(
                    "Please review this {lang} code for correctness, safety and clarity:\n\n```{lang}\n{code}\n```"
                )
            }
        }]
    }))
}
