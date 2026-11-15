//! Server-side MCP helpers.
//!
//! The wire shapes are reused from `zeroclaw_tools::mcp_protocol`, which is
//! the same module the MCP *client* speaks, so the server and the client can
//! never drift on the envelope. This module only adds the server-side result
//! builders and the request-shape helpers.

use serde_json::{Value, json};

/// Protocol version this server speaks by default.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Protocol versions this server accepts from a client.
///
/// The client states its preferred version during `initialize`. Echoing it back
/// keeps a newer client on its own dialect; the feature set implemented here
/// (`tools/list`, `tools/call`, `ping`) is valid across all three revisions.
pub const KNOWN_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// Choose the protocol version to answer `initialize` with.
pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(version) if KNOWN_PROTOCOL_VERSIONS.contains(&version) => {
            // SAFETY: every accepted version is a literal in KNOWN_PROTOCOL_VERSIONS.
            KNOWN_PROTOCOL_VERSIONS
                .iter()
                .find(|candidate| **candidate == version)
                .copied()
                .unwrap_or(MCP_PROTOCOL_VERSION)
        }
        _ => MCP_PROTOCOL_VERSION,
    }
}

/// A single text block in an MCP tool result.
pub fn text_content(text: impl Into<String>) -> Value {
    json!({ "type": "text", "text": text.into() })
}

/// Build a successful `tools/call` result.
pub fn tool_result(text: impl Into<String>) -> Value {
    json!({
        "content": [text_content(text)],
        "isError": false,
    })
}

/// Build a tool result that carries a notice beside its body.
///
/// The notice is its own content block, so the result body stays
/// exactly what the tool produced.
pub fn tool_result_with_notice(text: impl Into<String>, notice: &serde_json::Value) -> Value {
    let notice_text = notice
        .get("notice")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    json!({
        "content": [
            { "type": "text", "text": notice_text },
            text_content(text),
        ],
        "isError": false,
    })
}

/// Build a failed `tools/call` result.
///
/// MCP reports tool-level failures in the result body with `isError: true`
/// rather than as a JSON-RPC error, so the host can show the message to the
/// model and let it recover.
pub fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [text_content(message)],
        "isError": true,
    })
}

/// Read a string field from a JSON object.
pub fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Read an unsigned integer field from a JSON object.
pub fn usize_field(value: &Value, key: &str) -> Option<usize> {
    value.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_known_versions() {
        assert_eq!(negotiate_protocol_version(Some("2024-11-05")), "2024-11-05");
        assert_eq!(negotiate_protocol_version(Some("2025-06-18")), "2025-06-18");
    }

    #[test]
    fn falls_back_for_unknown_versions() {
        assert_eq!(
            negotiate_protocol_version(Some("1999-01-01")),
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(None), MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn tool_result_shape_matches_mcp() {
        let result = tool_result("hi");
        assert_eq!(result["isError"], json!(false));
        assert_eq!(result["content"][0]["type"], json!("text"));
        assert_eq!(result["content"][0]["text"], json!("hi"));
    }

    #[test]
    fn tool_error_shape_matches_mcp() {
        let result = tool_error("nope");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["content"][0]["text"], json!("nope"));
    }

    #[test]
    fn reads_typed_fields() {
        let value = json!({ "query": "x", "limit": 4 });
        assert_eq!(str_field(&value, "query"), Some("x"));
        assert_eq!(usize_field(&value, "limit"), Some(4));
        assert_eq!(usize_field(&value, "missing"), None);
    }
}
