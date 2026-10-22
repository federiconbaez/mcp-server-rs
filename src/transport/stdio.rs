//! stdio transport: one JSON-RPC message (or batch) per line.

use std::io::{BufRead, Write};

use crate::server::{Conn, Server};

/// Serve until `input` reaches EOF. Blank lines are ignored. Every output
/// message is written as a single line (serde_json never emits raw
/// newlines inside strings).
pub fn serve<R: BufRead, W: Write>(
    server: &Server,
    mut input: R,
    mut output: W,
) -> std::io::Result<()> {
    let mut conn = Conn::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let trimmed = trim(&line);
        if trimmed.is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_bytes(&mut conn, trimmed) {
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &b[start..end]
}
