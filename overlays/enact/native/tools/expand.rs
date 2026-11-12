//! `expand` — retrieve an archived tool output.
//!
//! When a tool returns more text than the conversation can carry, the runtime
//! keeps the whole result in the workspace archive and hands the model a
//! readable head plus a handle. `expand` is the other half: it gives the
//! whole thing back on demand, so the compression stays lossless.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

use crate::tool_archive::{self, ArchiveEntry};

/// Retrieve a previously archived tool output.
pub struct ExpandTool {
    security: Arc<SecurityPolicy>,
}

impl ExpandTool {
    /// Build the tool with the agent's security policy.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        tool_archive::use_workspace(&security.workspace_dir);
        Self { security }
    }

    /// The archive directories this agent may read from.
    ///
    /// The workspace's own archive is the primary one. The process-wide root
    /// is consulted as well, because a registry built for another agent in
    /// the same process publishes that one instead.
    fn roots(&self) -> Vec<std::path::PathBuf> {
        let mut roots = vec![tool_archive::archive_dir_for(&self.security.workspace_dir)];
        if let Some(published) = tool_archive::root()
            && !roots.contains(&published)
        {
            roots.push(published);
        }
        roots
    }
}

/// Render the archive listing.
fn render_listing(entries: &[ArchiveEntry]) -> String {
    if entries.is_empty() {
        return "The archive is empty.\n".to_string();
    }
    let mut out = format!("{} archived result(s):\n", entries.len());
    for entry in entries {
        out.push_str(&format!(
            "  {} — {} ({} characters)\n",
            entry.id, entry.tool, entry.chars
        ));
    }
    out
}

#[async_trait]
impl Tool for ExpandTool {
    fn name(&self) -> &str {
        "expand"
    }

    fn description(&self) -> &str {
        "Retrieve a tool output that was archived because it was too large for the conversation. The archived result carries a handle like `file_read-1a2b3c4d`; pass it as `id` to get the full text back. With no `id`, lists what the archive holds."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The archive handle to retrieve, e.g. `file_read-1a2b3c4d5e6f7081`."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let roots = self.roots();
        let Some(id) = args.get("id").and_then(|value| value.as_str()) else {
            let mut entries: Vec<ArchiveEntry> = Vec::new();
            for root in &roots {
                for entry in tool_archive::list(root) {
                    if !entries.iter().any(|existing| existing.id == entry.id) {
                        entries.push(entry);
                    }
                }
            }
            entries.sort_by(|a, b| b.id.cmp(&a.id));
            return Ok(ToolResult {
                success: true,
                output: ToolOutput::text(render_listing(&entries)),
                error: None,
            });
        };

        if !tool_archive::is_valid_handle(id) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::text(format!(
                    "'{id}' is not an archive handle. Handles look like `file_read-1a2b3c4d5e6f7081`."
                )),
                error: Some(format!("invalid archive handle: {id}")),
            });
        }

        for root in &roots {
            if let Some(text) = tool_archive::retrieve(root, id) {
                return Ok(ToolResult {
                    success: true,
                    output: ToolOutput::text(text),
                    error: None,
                });
            }
        }

        Ok(ToolResult {
            success: false,
            output: ToolOutput::text(format!(
                "No archived result with handle '{id}'. Call expand without an id to list what the archive holds."
            )),
            error: Some(format!("archived result not found: {id}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::tool::Tool;

    #[tokio::test]
    async fn an_empty_archive_lists_nothing() {
        let root = tempfile::tempdir().unwrap();
        let tool = ExpandTool {
            security: Arc::new(SecurityPolicy {
                workspace_dir: root.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
        };
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.as_str().contains("empty"));
    }

    #[tokio::test]
    async fn a_hand_that_is_not_a_handle_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let tool = ExpandTool {
            security: Arc::new(SecurityPolicy {
                workspace_dir: root.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
        };
        let result = tool.execute(json!({"id": "../../etc/passwd"})).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn an_archived_result_is_retrieved() {
        let root = tempfile::tempdir().unwrap();
        let archive_dir = tool_archive::archive_dir_for(root.path());
        std::fs::create_dir_all(&archive_dir).unwrap();
        std::fs::write(archive_dir.join("shell-abc.txt"), "the full output").unwrap();

        let tool = ExpandTool {
            security: Arc::new(SecurityPolicy {
                workspace_dir: root.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
        };
        let result = tool.execute(json!({"id": "shell-abc"})).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output.as_str(), "the full output");
    }

    #[tokio::test]
    async fn a_missing_handle_reports_the_listing_hint() {
        let root = tempfile::tempdir().unwrap();
        let tool = ExpandTool {
            security: Arc::new(SecurityPolicy {
                workspace_dir: root.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
        };
        let result = tool.execute(json!({"id": "shell-abc"})).await.unwrap();
        assert!(!result.success);
        assert!(result.output.as_str().contains("without an id"));
    }
}
