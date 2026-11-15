//! MCP server for the Enact built-in tool registry.
//!
//! Codex CLI and Claude Code both speak MCP over stdio. This module exposes
//! Enact's own tool registry to them, so one registry serves every host
//! instead of each host owning a different tool set.
//!
//! The surface is deliberately small: `mcp_search` finds a tool by intent and
//! `mcp_call` runs it. A host that wants the flat surface can ask for it, but
//! advertising every schema eagerly would spend the model's context on tools
//! it will never call.

pub mod bootstrap;
pub mod cli;
pub mod config_discovery;
pub mod http;
pub mod protocol;
pub mod server;
pub mod surface;

pub use http::{DEFAULT_HTTP_PORT, serve_http};
pub use server::{CALL_TOOL, Expose, McpServer, SEARCH_TOOL, serve_stdio};
pub use surface::ToolSurface;
