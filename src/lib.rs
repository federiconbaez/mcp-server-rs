//! # mcp-server-rs
//!
//! A Model Context Protocol server implemented from scratch (no official
//! SDK). See the README for the supported protocol revisions and the list
//! of what is and is not implemented.

pub mod calc;
pub mod jsonrpc;
pub mod protocol;
#[cfg(target_os = "linux")]
pub mod sandbox;
