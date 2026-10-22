#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub const BIN: &str = env!("CARGO_BIN_EXE_mcp-server-rs");
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const MODERN: &str = "2026-07-28";
pub const PV_KEY: &str = "io.modelcontextprotocol/protocolVersion";

/// A server child process driven over stdio.
pub struct StdioClient {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl StdioClient {
    pub fn spawn() -> Self {
        let mut child = Command::new(BIN)
            .arg("stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            child,
            stdin,
            lines: rx,
        }
    }

    /// Spawn and complete the legacy handshake with `version`.
    pub fn initialized(version: &str) -> Self {
        let mut c = Self::spawn();
        let r = c.call(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion": version, "capabilities": {}, "clientInfo": {"name":"test","version":"0"}}}));
        assert_eq!(r["result"]["protocolVersion"], version, "{r}");
        c.send_value(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        c
    }

    pub fn send_raw(&mut self, raw: &[u8]) {
        let s = self.stdin.as_mut().unwrap();
        s.write_all(raw).unwrap();
        s.write_all(b"\n").unwrap();
        s.flush().unwrap();
    }

    pub fn send_value(&mut self, v: &Value) {
        self.send_raw(v.to_string().as_bytes());
    }

    pub fn recv(&mut self) -> Value {
        let line = self.lines.recv_timeout(TIMEOUT).expect("response line");
        assert!(!line.contains('\n'));
        serde_json::from_str(&line).expect("valid JSON on stdout")
    }

    /// Assert nothing arrives within a short window.
    pub fn expect_silence(&mut self) {
        assert!(self.lines.recv_timeout(Duration::from_millis(300)).is_err());
    }

    pub fn call(&mut self, v: Value) -> Value {
        self.send_value(&v);
        self.recv()
    }

    pub fn raw_call(&mut self, raw: &str) -> Value {
        self.send_raw(raw.as_bytes());
        self.recv()
    }

    /// Close stdin and wait for exit; returns whether it exited successfully.
    pub fn close(mut self) -> bool {
        drop(self.stdin.take());
        for _ in 0..200 {
            if let Some(st) = self.child.try_wait().unwrap() {
                return st.success();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        false
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn req(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

/// Params carrying modern (2026-07-28) request metadata.
pub fn modern_params(mut extra: Value) -> Value {
    let meta = json!({
        PV_KEY: MODERN,
        "io.modelcontextprotocol/clientInfo": {"name":"test","version":"0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    extra.as_object_mut().unwrap().insert("_meta".into(), meta);
    extra
}

pub fn err_code(v: &Value) -> i64 {
    v["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("expected error, got {v}"))
}

/// Minimal HTTP/1.1 response.
#[derive(Debug)]
pub struct Resp {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {:?}", self.body))
    }
}

pub fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Resp {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let body = if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
        dechunk(rest)
    } else {
        rest.to_string()
    };
    Resp {
        status,
        headers,
        body,
    }
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        s = &rest[n + 2..];
    }
    out
}

pub const JSON_HEADERS: &[(&str, &str)] = &[
    ("Content-Type", "application/json"),
    ("Accept", "application/json, text/event-stream"),
];
