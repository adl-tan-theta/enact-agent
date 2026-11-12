//! `checkpoint` — mark a point in the conversation the agent can return to.
//!
//! An agent that has tried three approaches and is about to try a fourth has
//! no way back to the first attempt: the tool contract gives a tool no view of
//! the conversation, and the transcript belongs to the turn loop. `checkpoint`
//! is the agent's half of that gap — it asks the loop to remember where the
//! conversation stood, and `rewind` asks the loop to go back there.

use async_trait::async_trait;
use serde_json::{Value, json};

use zeroclaw_api::tool::{Tool, ToolResult};

/// Mark the current point of the conversation.
pub struct CheckpointTool;

impl CheckpointTool {
    /// Build the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CheckpointTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CheckpointTool {
    fn name(&self) -> &str {
        "checkpoint"
    }

    fn description(&self) -> &str {
        "Mark the current point of the conversation as a checkpoint, so that a later `rewind` can return to it. Give it a short label that says what the conversation had reached. The checkpoint is taken at the current tool round: rewinding to it discards that round and everything after it."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "What the conversation had reached at this point, in a few words. Optional."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let label = args
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("checkpoint")
            .to_string();

        let control = crate::agent::turn_control::current()?;
        let checkpoint = {
            let mut state = match control.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.record_checkpoint(&label)
        };

        Ok(ToolResult::ok(format!(
            "Checkpoint `{}` recorded: {label}. The conversation can be returned to it with `rewind`.",
            checkpoint.id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::turn_control::{
        AgentMode, TranscriptAnchor, TurnControl, scope,
    };

    #[tokio::test]
    async fn a_checkpoint_is_recorded_with_its_label() {
        let handle = TurnControl::new(AgentMode::Execute);
        let tool = CheckpointTool::new();
        let result = scope(
            std::sync::Arc::clone(&handle),
            tool.execute(json!({"label": "before the refactor"})),
        )
        .await
        .unwrap();
        assert!(result.success);
        assert!(result.output.as_str().contains("before the refactor"), "{result:?}");
    }

    #[tokio::test]
    async fn a_checkpoint_without_a_label_still_works() {
        let handle = TurnControl::new(AgentMode::Execute);
        let tool = CheckpointTool::new();
        let result = scope(std::sync::Arc::clone(&handle), tool.execute(json!({})))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.as_str().contains("checkpoint"), "{result:?}");
    }

    #[tokio::test]
    async fn a_checkpoint_records_the_transcript_position() {
        let handle = TurnControl::new(AgentMode::Execute);
        {
            let mut state = match handle.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.publish_anchor(TranscriptAnchor {
                history_len: 11,
                canonical_len: 4,
            });
        }
        scope(
            std::sync::Arc::clone(&handle),
            CheckpointTool::new().execute(json!({"label": "here"})),
        )
        .await
        .unwrap();
        let state = match handle.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let checkpoint = state.latest_checkpoint().expect("a checkpoint");
        assert_eq!(checkpoint.anchor.history_len, 11);
        assert_eq!(checkpoint.anchor.canonical_len, 4);
    }

    #[tokio::test]
    async fn outside_a_turn_the_tool_refuses() {
        let error = CheckpointTool::new().execute(json!({})).await.unwrap_err();
        assert!(
            error.to_string().contains("inside an active agent turn"),
            "{error}"
        );
    }
}
