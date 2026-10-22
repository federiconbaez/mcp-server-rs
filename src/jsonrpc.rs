//! Minimal, dependency-light JSON-RPC 2.0 layer.
//!
//! This module only knows about JSON-RPC 2.0 framing rules. MCP-specific
//! semantics (lifecycle, protocol versions, batching policy) live in
//! [`crate::server`].

use serde_json::{json, Map, Value};

/// Invalid JSON was received by the server.
pub const PARSE_ERROR: i64 = -32700;
/// The JSON sent is not a valid Request object.
pub const INVALID_REQUEST: i64 = -32600;
/// The method does not exist / is not available.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Invalid method parameter(s).
pub const INVALID_PARAMS: i64 = -32602;
/// Internal JSON-RPC error.
pub const INTERNAL_ERROR: i64 = -32603;

/// MCP 2026-07-28: HTTP headers do not match the body.
pub const HEADER_MISMATCH: i64 = -32020;
/// MCP 2026-07-28: a required client capability is missing.
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;
/// MCP 2026-07-28: requested protocol version is not supported.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// MCP 2025-11-25 and earlier: resource not found.
pub const LEGACY_RESOURCE_NOT_FOUND: i64 = -32002;

/// A JSON-RPC request id. MCP restricts ids to strings or integers and
/// forbids `null`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Id {
    Str(String),
    Int(i64),
}

impl Id {
    pub fn to_value(&self) -> Value {
        match self {
            Id::Str(s) => Value::String(s.clone()),
            Id::Int(n) => Value::from(*n),
        }
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn parse_error() -> Self {
        Self::new(PARSE_ERROR, "Parse error")
    }

    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::new(
            INVALID_REQUEST,
            format!("Invalid Request: {}", detail.into()),
        )
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("Method not found: {method}"))
    }

    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, format!("Invalid params: {}", detail.into()))
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, format!("Internal error: {}", detail.into()))
    }

    pub fn to_value(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("code".into(), Value::from(self.code));
        obj.insert("message".into(), Value::String(self.message.clone()));
        if let Some(data) = &self.data {
            obj.insert("data".into(), data.clone());
        }
        Value::Object(obj)
    }
}

/// A validated incoming JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request {
        id: Id,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    /// A response (result or error) sent by the peer. This server never
    /// issues requests, so responses are accepted and ignored.
    Response,
}

impl Incoming {
    pub fn method(&self) -> Option<&str> {
        match self {
            Incoming::Request { method, .. } | Incoming::Notification { method, .. } => {
                Some(method)
            }
            Incoming::Response => None,
        }
    }

    pub fn params(&self) -> Option<&Value> {
        match self {
            Incoming::Request { params, .. } | Incoming::Notification { params, .. } => {
                params.as_ref()
            }
            Incoming::Response => None,
        }
    }
}

/// Best-effort extraction of a usable id from an invalid message, so the
/// error response can be correlated when possible.
pub fn salvage_id(v: &Value) -> Value {
    match v.get("id") {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Number(n)) if n.is_i64() => Value::Number(n.clone()),
        _ => Value::Null,
    }
}

/// Validate a single (non-batch) JSON value as a JSON-RPC 2.0 message.
///
/// On failure returns the error together with the id to use in the
/// response (`null` when it cannot be determined).
pub fn validate(v: &Value) -> Result<Incoming, (Value, RpcError)> {
    let obj = match v.as_object() {
        Some(o) => o,
        None => {
            return Err((
                Value::Null,
                RpcError::invalid_request("message must be a JSON object"),
            ))
        }
    };
    let fail = |msg: &str| Err((salvage_id(v), RpcError::invalid_request(msg)));

    match obj.get("jsonrpc") {
        Some(Value::String(s)) if s == "2.0" => {}
        _ => return fail("\"jsonrpc\" member must be exactly \"2.0\""),
    }

    let has_method = obj.contains_key("method");
    let has_result = obj.contains_key("result");
    let has_error = obj.contains_key("error");

    if !has_method {
        if has_result ^ has_error && obj.contains_key("id") {
            return Ok(Incoming::Response);
        }
        return fail("missing \"method\" member");
    }
    if has_result || has_error {
        return fail("a request must not contain \"result\" or \"error\"");
    }

    let method = match obj.get("method") {
        Some(Value::String(m)) => m.clone(),
        _ => return fail("\"method\" must be a string"),
    };

    let params = match obj.get("params") {
        None => None,
        Some(p @ (Value::Object(_) | Value::Array(_))) => Some(p.clone()),
        Some(_) => return fail("\"params\" must be an object or an array"),
    };

    match obj.get("id") {
        None => Ok(Incoming::Notification { method, params }),
        Some(Value::String(s)) => Ok(Incoming::Request {
            id: Id::Str(s.clone()),
            method,
            params,
        }),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => Ok(Incoming::Request {
                id: Id::Int(i),
                method,
                params,
            }),
            None => Err((
                Value::Null,
                RpcError::invalid_request("\"id\" must be a string or an integer"),
            )),
        },
        Some(Value::Null) => Err((
            Value::Null,
            RpcError::invalid_request("\"id\" must not be null in MCP"),
        )),
        Some(_) => Err((
            Value::Null,
            RpcError::invalid_request("\"id\" must be a string or an integer"),
        )),
    }
}

/// Build a success response.
pub fn response(id: &Id, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.to_value(), "result": result })
}

/// Build an error response. `id` is `Value::Null` when unknown.
pub fn error_response(id: Value, err: &RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": err.to_value() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_request_and_notification() {
        let r = validate(&json!({"jsonrpc":"2.0","id":1,"method":"ping"})).unwrap();
        assert!(matches!(r, Incoming::Request { id: Id::Int(1), .. }));
        let n = validate(&json!({"jsonrpc":"2.0","method":"x"})).unwrap();
        assert!(matches!(n, Incoming::Notification { .. }));
    }

    #[test]
    fn rejects_wrong_version_and_types() {
        let (id, e) = validate(&json!({"jsonrpc":"1.0","id":"a","method":"x"})).unwrap_err();
        assert_eq!(id, json!("a"));
        assert_eq!(e.code, INVALID_REQUEST);
        let (_, e) = validate(&json!({"jsonrpc":"2.0","id":1,"method":5})).unwrap_err();
        assert_eq!(e.code, INVALID_REQUEST);
        let (_, e) =
            validate(&json!({"jsonrpc":"2.0","id":1,"method":"x","params":3})).unwrap_err();
        assert_eq!(e.code, INVALID_REQUEST);
        let (id, e) = validate(&json!({"jsonrpc":"2.0","id":1.5,"method":"x"})).unwrap_err();
        assert_eq!((id, e.code), (Value::Null, INVALID_REQUEST));
        let (_, e) = validate(&json!({"jsonrpc":"2.0","id":null,"method":"x"})).unwrap_err();
        assert_eq!(e.code, INVALID_REQUEST);
        let (_, e) = validate(&json!(42)).unwrap_err();
        assert_eq!(e.code, INVALID_REQUEST);
    }

    #[test]
    fn recognises_responses() {
        assert_eq!(
            validate(&json!({"jsonrpc":"2.0","id":1,"result":{}})).unwrap(),
            Incoming::Response
        );
        assert!(validate(&json!({"jsonrpc":"2.0","id":1,"result":{},"error":{}})).is_err());
    }
}
