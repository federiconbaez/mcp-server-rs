//! Protocol revision constants.

/// Modern (stateless, per-request `_meta`) revision.
pub const MODERN_VERSION: &str = "2026-07-28";

/// Legacy (handshake-based) revisions, newest first.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// Revision assumed for a legacy HTTP request that omits
/// `MCP-Protocol-Version` (spec: SHOULD assume 2025-03-26).
pub const HTTP_DEFAULT_LEGACY_VERSION: &str = "2025-03-26";

/// The only legacy revision that allows JSON-RPC batches.
pub const BATCH_VERSION: &str = "2025-03-26";

/// `_meta` keys defined by the 2026-07-28 revision.
pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

pub const SERVER_NAME: &str = "mcp-server-rs";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Freshness hint for cacheable results in the modern revision.
pub const LIST_TTL_MS: u64 = 60_000;

/// Every version this server understands, newest first.
pub fn all_versions() -> Vec<&'static str> {
    let mut v = vec![MODERN_VERSION];
    v.extend_from_slice(LEGACY_VERSIONS);
    v
}

pub fn is_legacy(v: &str) -> bool {
    LEGACY_VERSIONS.contains(&v)
}

pub fn is_supported(v: &str) -> bool {
    v == MODERN_VERSION || is_legacy(v)
}
