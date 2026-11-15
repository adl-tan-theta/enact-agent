//! `rewind` — return the conversation to a checkpoint.
//!
//! The other half of `checkpoint`. The agent asks the turn loop to restore the
//! transcript to a marked point; the loop owns the transcript, so it applies
//! the request itself at the top of its next iteration, where it can restore
//! both buffers the checkpoint recorded.

use async_trait::async_trait;
use serde_json::{Value, json};

use zeroclaw_api::tool::{Tool, ToolResult};

/// Return the conversation to a checkpoint.
pub struct RewindTool;

impl RewindTool {
    /// Build the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for RewindTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for RewindTool {
    fn name(&self) -> &str {
        "rewind"
    }

    fn description(&self) -> &str {
        "Return the conversation to a checkpoint taken with `checkpoint`. Everything said and done after that checkpoint is discarded, and the conversation continues from the marked point. With no checkpoint named, the most recent checkpoint is used."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "checkpoint_id": {
                    "type": "string",
                    "description": "The checkpoint to return to, as reported by `checkpoint`. Optional; the most recent checkpoint is used when it is omitted."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let requested = args
            .get("checkpoint_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty());

        let control = crate::agent::turn_control::current()?;
        let (id, label) = {
            let state = match control.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            let checkpoint = match requested {
                Some(id) => state.checkpoint(id),
                None => state.latest_checkpoint(),
            };
            let Some(checkpoint) = checkpoint else {
                return Ok(ToolResult::err(match requested {
                    Some(id) => format!(
                        "No checkpoint `{id}` was taken in this turn. Take one with `checkpoint` first."
                    ),
                    None => "No checkpoint has been taken in this turn. Take one with `checkpoint` first."
                        .to_string(),
                }));
            };
            (checkpoint.id.clone(), checkpoint.label.clone())
        };

        {
            let mut state = match control.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.request_rewind(&id);
        }

        Ok(ToolResult::ok(format!(
            "Returning the conversation to checkpoint `{id}` ({label}). The transcript is restored \
             there before the next step runs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::turn_control::{AgentMode, TurnControl, TurnRequest, scope};
    use std::sync::Arc;

    #[tokio::test]
    async fn a_rewind_to_the_latest_checkpoint_is_requested() {
        let handle = TurnControl::new(AgentMode::Execute);
        {
            let mut state = match handle.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.record_checkpoint("before the refactor");
        }
        let result = scope(
            Arc::clone(&handle),
            RewindTool::new().execute(json!({})),
        )
        .await
        .unwrap();
        assert!(result.success);
        assert!(result.output.as_str().contains("before the refactor"), "{result:?}");

        let requests = {
            let mut state = match handle.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.take_requests()
        };
        assert_eq!(requests.len(), 1);
        assert!(matches!(&requests[0], TurnRequest::Rewind { checkpoint_id } if checkpoint_id == "cp1"));
    }

    #[tokio::test]
    async fn a_named_checkpoint_is_rewound_to() {
        let handle = TurnControl::new(AgentMode::Execute);
        {
            let mut state = match handle.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.record_checkpoint("first");
            state.record_checkpoint("second");
        }
        scope(
            Arc::clone(&handle),
            RewindTool::new().execute(json!({"checkpoint_id": "cp1"})),
        )
        .await
        .unwrap();
        let requests = {
            let mut state = match handle.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.take_requests()
        };
        assert!(matches!(&requests[0], TurnRequest::Rewind { checkpoint_id } if checkpoint_id == "cp1"));
    }

    #[tokio::test]
    async fn a_rewind_without_a_checkpoint_is_refused() {
        let handle = TurnControl::new(AgentMode::Execute);
        let result = scope(Arc::clone(&handle), RewindTool::new().execute(json!({})))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or_default().contains("No checkpoint"),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_rewind_to_an_unknown_checkpoint_is_refused() {
        let handle = TurnControl::new(AgentMode::Execute);
        let result = scope(
            Arc::clone(&handle),
            RewindTool::new().execute(json!({"checkpoint_id": "cp9"})),
        )
        .await
        .unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or_default().contains("cp9"),
            "{result:?}"
        );
    }
}
