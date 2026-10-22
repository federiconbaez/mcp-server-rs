//! Tool registry, a minimal JSON-Schema subset validator and built-in tools.

use serde_json::{json, Value};
use std::path::PathBuf;

/// Result of executing a tool. `Err` becomes a tool execution error
/// (`isError: true`), not a JSON-RPC protocol error.
pub type ToolResult = Result<Value, String>;

/// Context available to tools while executing.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Binary used to spawn sandbox workers.
    pub worker_exe: Option<PathBuf>,
}

/// A tool exposed through `tools/list` / `tools/call`.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    /// Full MCP `Tool` definition (name, description, inputSchema, ...).
    fn definition(&self) -> Value;
    /// Execute. `args` has already been validated against `inputSchema`.
    /// Returns a full `CallToolResult` body on success, or an error message.
    fn call(&self, args: &Value, ctx: &ToolContext) -> ToolResult;
}

/// Validate `value` against the subset of JSON Schema used by our tools:
/// `type` (object/string/number/integer/boolean), `properties`, `required`,
/// `additionalProperties: false`, `maxLength`.
pub fn validate_schema(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(t) = schema.get("type").and_then(Value::as_str) {
        let ok = match t {
            "object" => value.is_object(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            _ => true,
        };
        if !ok {
            return Err(format!("{path}: expected {t}"));
        }
    }
    if let (Some(max), Some(s)) = (
        schema.get("maxLength").and_then(Value::as_u64),
        value.as_str(),
    ) {
        if s.chars().count() as u64 > max {
            return Err(format!("{path}: longer than {max} characters"));
        }
    }
    if let Some(obj) = value.as_object() {
        let props = schema.get("properties").and_then(Value::as_object);
        if let Some(req) = schema.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    return Err(format!("{path}: missing required property '{r}'"));
                }
            }
        }
        for (k, v) in obj {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => validate_schema(sub, v, &format!("{path}.{k}"))?,
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{path}: unexpected property '{k}'"))
                }
                None => {}
            }
        }
    }
    Ok(())
}

pub fn text_result(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": false })
}

/// `echo`: returns its input. Useful for connectivity checks.
pub struct Echo;

impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn definition(&self) -> Value {
        json!({
            "name": "echo",
            "title": "Echo",
            "description": "Returns the given text unchanged.",
            "inputSchema": {
                "type": "object",
                "properties": { "text": { "type": "string", "maxLength": 10000 } },
                "required": ["text"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        })
    }
    fn call(&self, args: &Value, _ctx: &ToolContext) -> ToolResult {
        Ok(text_result(args["text"].as_str().unwrap_or_default()))
    }
}

/// `calc`: evaluates an arithmetic expression inside a seccomp sandbox.
#[cfg(target_os = "linux")]
pub struct Calc;

#[cfg(target_os = "linux")]
impl Tool for Calc {
    fn name(&self) -> &str {
        "calc"
    }
    fn definition(&self) -> Value {
        json!({
            "name": "calc",
            "title": "Sandboxed calculator",
            "description": "Evaluates an arithmetic expression (+ - * / % ^ and parentheses) in a short-lived child process restricted by a seccomp-BPF allow-list and rlimits.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "expression": { "type": "string", "maxLength": crate::calc::MAX_INPUT }
                },
                "required": ["expression"],
                "additionalProperties": false
            },
            "outputSchema": {
                "type": "object",
                "properties": { "value": { "type": "number" } },
                "required": ["value"]
            },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        })
    }
    fn call(&self, args: &Value, ctx: &ToolContext) -> ToolResult {
        let expr = args["expression"].as_str().unwrap_or_default();
        let exe = match &ctx.worker_exe {
            Some(p) => p.clone(),
            None => std::env::current_exe().map_err(|e| e.to_string())?,
        };
        let reply =
            crate::sandbox::run_job(&exe, "calc", expr.as_bytes()).map_err(|e| e.to_string())?;
        if let Some(v) = reply.get("ok").and_then(Value::as_f64) {
            Ok(json!({
                "content": [{ "type": "text", "text": format_number(v) }],
                "structuredContent": { "value": v },
                "isError": false
            }))
        } else {
            Err(reply
                .get("err")
                .and_then(Value::as_str)
                .unwrap_or("unknown sandbox error")
                .to_string())
        }
    }
}

/// Render integers without a trailing `.0`.
pub fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Built-in tools in deterministic order.
pub fn builtin() -> Vec<Box<dyn Tool>> {
    vec![
        #[cfg(target_os = "linux")]
        Box::new(Calc),
        Box::new(Echo),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_validation() {
        let s = Echo.definition()["inputSchema"].clone();
        assert!(validate_schema(&s, &json!({"text":"hi"}), "arguments").is_ok());
        assert!(validate_schema(&s, &json!({}), "arguments").is_err());
        assert!(validate_schema(&s, &json!({"text":1}), "arguments").is_err());
        assert!(validate_schema(&s, &json!({"text":"a","x":1}), "arguments").is_err());
        assert!(validate_schema(&s, &json!([1]), "arguments").is_err());
    }

    #[test]
    fn number_format() {
        assert_eq!(format_number(7.0), "7");
        assert_eq!(format_number(0.5), "0.5");
    }
}
