//! The MCP server: JSON-RPC 2.0 over stdio.
//!
//! The stdio transport is newline-delimited JSON, matching the framing the
//! existing MCP client in `zeroclaw-tools` already speaks.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::mcp_server::protocol::{
    negotiate_protocol_version, str_field, tool_error, tool_result, tool_result_with_notice,
    usize_field,
};
use crate::mcp_server::surface::{DEFAULT_SEARCH_LIMIT, ToolSurface};

/// The canonical search tool name.
pub const SEARCH_TOOL: &str = "mcp_search";
/// The canonical call tool name.
pub const CALL_TOOL: &str = "mcp_call";

/// How much of the registry the host sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expose {
    /// Only `mcp_search` and `mcp_call`; the model pulls schemas on demand.
    Minimal,
    /// Every tool in the registry, with its real schema.
    Full,
}

/// JSON schema advertised for `mcp_search`.
pub fn search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "What you want to do, in plain language (e.g. 'read a file')."
            },
            "tool": {
                "type": "string",
                "description": "Exact tool name. When given, the full schema is returned."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 32,
                "description": "Maximum number of tool references to return."
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

/// JSON schema advertised for `mcp_call`.
pub fn call_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tool": {
                "type": "string",
                "description": "Tool name, as returned by mcp_search."
            },
            "action": {
                "type": "string",
                "description": "Operation to perform, when the tool declares one."
            },
            "arguments": {
                "type": "object",
                "description": "Arguments for the tool.",
                "default": {}
            }
        },
        "required": ["tool"],
        "additionalProperties": false
    })
}

/// Instructions returned with `initialize`.
pub fn instructions() -> String {
    format!(
        "Enact's built-in tool registry, served over MCP. {SEARCH_TOOL} finds a tool by intent; \
         {CALL_TOOL} runs it. Use {SEARCH_TOOL} before guessing a tool name."
    )
}

/// An MCP server bound to a tool surface.
pub struct McpServer {
    surface: ToolSurface,
    expose: Expose,
}

impl McpServer {
    /// Build a server over a tool surface.
    pub fn new(surface: ToolSurface, expose: Expose) -> Self {
        Self { surface, expose }
    }

    /// The registry this server exposes.
    pub fn surface(&self) -> &ToolSurface {
        &self.surface
    }

    /// Handle one JSON-RPC message.
    ///
    /// Returns `None` for notifications, which never get a response.
    pub async fn handle(&self, message: Value) -> Option<Value> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

        let Some(method) = method else {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32600, "message": "invalid request: missing method" }
            }));
        };

        let result = match method.as_str() {
            "initialize" => Ok(self.initialize(&params)),
            "notifications/initialized" | "initialized" => return None,
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.list_tools()),
            "tools/call" => Ok(self.call_tool(&params).await),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "resources/list" => Ok(json!({ "resources": [] })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "logging/setLevel" => Ok(json!({})),
            other => Err(other.to_string()),
        };

        // A notification carries no id and never gets a response.
        let Some(id) = id else {
            return None;
        };

        match result {
            Ok(result) => Some(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
            Err(method) => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            })),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = str_field(params, "protocolVersion");
        json!({
            "protocolVersion": negotiate_protocol_version(requested),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "enact-agent", "version": env!("CARGO_PKG_VERSION") },
            "instructions": instructions(),
        })
    }

    fn list_tools(&self) -> Value {
        let mut tools = vec![
            json!({
                "name": SEARCH_TOOL,
                "description": "Find a tool by intent. Returns the tool name, its parameters and a summary.",
                "inputSchema": search_schema(),
            }),
            json!({
                "name": CALL_TOOL,
                "description": "Call a tool. Use the name returned by mcp_search.",
                "inputSchema": call_schema(),
            }),
        ];
        if self.expose == Expose::Full {
            for descriptor in self.surface.descriptors() {
                tools.push(json!({
                    "name": descriptor.name,
                    "description": descriptor.description,
                    "inputSchema": descriptor.input_schema,
                }));
            }
        }
        json!({ "tools": tools })
    }

    async fn call_tool(&self, params: &Value) -> Value {
        let Some(name) = str_field(params, "name") else {
            return tool_error("tool call is missing 'name'");
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return tool_error("tool call 'arguments' must be an object");
        }

        match name {
            SEARCH_TOOL => self.run_search(&arguments),
            CALL_TOOL => self.run_call(&arguments).await,
            other => {
                // A host may address a registry tool directly when the full
                // surface is exposed.
                if self.surface.get(other).is_some() {
                    self.execute(other, arguments).await
                } else {
                    tool_error(format!(
                        "unknown tool {other:?}; use {SEARCH_TOOL} to find the right name"
                    ))
                }
            }
        }
    }

    fn run_search(&self, arguments: &Value) -> Value {
        if let Some(name) = str_field(arguments, "tool") {
            return match self.surface.detail(name) {
                Some(detail) => tool_result(detail.to_string()),
                None => tool_error(format!("unknown tool {name:?}")),
            };
        }
        let query = str_field(arguments, "query").unwrap_or_default();
        let limit = usize_field(arguments, "limit").unwrap_or(DEFAULT_SEARCH_LIMIT);
        tool_result(self.surface.search(query, limit).to_string())
    }

    async fn run_call(&self, arguments: &Value) -> Value {
        let Some(tool) = str_field(arguments, "tool") else {
            return tool_error(format!("{CALL_TOOL} requires a 'tool'"));
        };
        let mut call_arguments = arguments
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !call_arguments.is_object() {
            return tool_error(format!("{CALL_TOOL} 'arguments' must be an object"));
        }
        // Forward `action` only when the target tool declares it, so a tool
        // that does not model an action never receives an unknown parameter.
        if let (Some(action), Some(object)) = (
            str_field(arguments, "action"),
            call_arguments.as_object_mut(),
        ) {
            if self.declares_action(tool) {
                object.insert("action".to_string(), json!(action));
            }
        }
        self.execute(tool, call_arguments).await
    }

    fn declares_action(&self, tool: &str) -> bool {
        let Some(schema) = self.surface.get(tool).map(|tool| tool.parameters_schema()) else {
            return false;
        };
        schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.contains_key("action"))
            .unwrap_or(false)
    }

    async fn execute(&self, tool: &str, arguments: Value) -> Value {
        match self.surface.call(tool, arguments).await {
            Ok(payload) => {
                // A deprecation notice travels beside the result body,
                // not inside it: the body stays exactly what the tool
                // produced, and the notice is its own content block.
                let notice = payload
                    .get("data")
                    .and_then(|data| data.get("deprecation"))
                    .cloned();
                let mut body = payload.clone();
                if let Some(object) = body
                    .get_mut("data")
                    .and_then(|data| data.as_object_mut())
                {
                    object.remove("deprecation");
                }
                let text = body.to_string();
                if payload["success"].as_bool() == Some(false) {
                    tool_error(text)
                } else {
                    match notice {
                        Some(notice) => tool_result_with_notice(text, &notice),
                        None => tool_result(text),
                    }
                }
            }
            Err(error) => tool_error(error),
        }
    }
}

/// Serve MCP over stdio until the input stream closes.
pub async fn serve_stdio(server: McpServer) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let stdout = tokio::io::stdout();
    let mut writer = stdout;

    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(message) => server.handle(message).await,
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": format!("parse error: {error}") }
            })),
        };
        if let Some(response) = response {
            writer.write_all(response.to_string().as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_server::surface::ToolSurface;
    use std::sync::Arc;
    use zeroclaw_api::attribution::{Role, ToolProvenance};
    use zeroclaw_api::tool::{Tool, ToolResult};

    struct FakeTool;

    impl zeroclaw_api::attribution::Attributable for FakeTool {
        fn role(&self) -> Role {
            Role::Agent
        }
        fn alias(&self) -> &str {
            "fake"
        }
        fn tool_provenance(&self) -> ToolProvenance {
            ToolProvenance::Extension
        }
    }

    #[async_trait::async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            "file_read"
        }
        fn description(&self) -> &str {
            "Read a workspace file."
        }
        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            })
        }
        async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok("read"))
        }
    }

    fn server(expose: Expose) -> McpServer {
        let surface = ToolSurface::new(vec![Arc::new(FakeTool) as Arc<dyn Tool>]);
        McpServer::new(surface, expose)
    }

    #[tokio::test]
    async fn initialize_advertises_the_protocol() {
        let response = server(Expose::Minimal)
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                            "clientInfo": { "name": "codex-cli", "version": "1" } }
            }))
            .await
            .unwrap();
        assert_eq!(response["result"]["protocolVersion"], json!("2024-11-05"));
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert_eq!(
            response["result"]["serverInfo"]["name"],
            json!("enact-agent")
        );
    }

    #[tokio::test]
    async fn initialize_echoes_a_known_newer_protocol() {
        let response = server(Expose::Minimal)
            .handle(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18" }
            }))
            .await
            .unwrap();
        assert_eq!(response["result"]["protocolVersion"], json!("2025-06-18"));
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        assert!(
            server(Expose::Minimal)
                .handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn minimal_surface_lists_two_tools() {
        let response = server(Expose::Minimal)
            .handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .await
            .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], json!(SEARCH_TOOL));
        assert_eq!(tools[1]["name"], json!(CALL_TOOL));
    }

    #[tokio::test]
    async fn full_surface_lists_the_registry() {
        let response = server(Expose::Full)
            .handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .await
            .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[2]["name"], json!("file_read"));
    }

    #[tokio::test]
    async fn unknown_method_is_an_error() {
        let response = server(Expose::Minimal)
            .handle(json!({ "jsonrpc": "2.0", "id": 3, "method": "nope" }))
            .await
            .unwrap();
        assert_eq!(response["error"]["code"], json!(-32601));
    }

    #[tokio::test]
    async fn search_then_call_round_trip() {
        let found = server(Expose::Minimal)
            .handle(json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": SEARCH_TOOL, "arguments": { "query": "read a file" } }
            }))
            .await
            .unwrap();
        assert_eq!(found["result"]["isError"], json!(false));

        let called = server(Expose::Minimal)
            .handle(json!({
                "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": { "name": CALL_TOOL, "arguments": { "tool": "file_read",
                          "arguments": { "path": "x" } } }
            }))
            .await
            .unwrap();
        assert_eq!(called["result"]["isError"], json!(false));
        assert!(
            called["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("read")
        );
    }

    #[tokio::test]
    async fn unknown_tool_is_reported_in_the_result_body() {
        let response = server(Expose::Minimal)
            .handle(json!({
                "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                "params": { "name": CALL_TOOL, "arguments": { "tool": "nope" } }
            }))
            .await
            .unwrap();
        assert_eq!(response["result"]["isError"], json!(true));
    }

    #[tokio::test]
    async fn a_registry_tool_can_be_called_directly() {
        let response = server(Expose::Full)
            .handle(json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": { "name": "file_read", "arguments": { "path": "x" } }
            }))
            .await
            .unwrap();
        assert_eq!(response["result"]["isError"], json!(false));
    }
}
