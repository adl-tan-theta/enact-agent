//! `plan_execute` — ask the operator to let the turn act again.
//!
//! Leaving plan mode is the one decision the agent may not take for itself:
//! plan mode exists because the operator wants the plan before the change. So
//! the request is a write-tier tool call, which the approval policy judges
//! like any other write — in supervised mode the operator is asked, and only
//! their approval lets the turn leave plan mode.

use async_trait::async_trait;
use serde_json::{Value, json};

use zeroclaw_api::tool::{Tool, ToolResult};

/// Ask to leave plan mode and act.
pub struct PlanExecuteTool;

impl PlanExecuteTool {
    /// Build the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for PlanExecuteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PlanExecuteTool {
    fn name(&self) -> &str {
        "plan_execute"
    }

    fn description(&self) -> &str {
        "Ask to leave plan mode and start changing things. The operator is asked to approve the request; the turn stays in plan mode until they do. Write the plan down with TodoWrite first, so the operator can see what the approval covers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "What the turn intends to do once execution is allowed. Optional."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let summary = args
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty());

        let control = crate::agent::turn_control::current()?;
        {
            let mut state = match control.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.request_mode(crate::agent::turn_control::AgentMode::Execute);
        }

        let mut report =
            "Execution requested. If the operator approves, the turn leaves plan mode and may act from the next step on."
                .to_string();
        if let Some(summary) = summary {
            report.push_str(&format!("\nThe plan was: {summary}"));
        }
        Ok(ToolResult::ok(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::turn_control::{AgentMode, TurnControl, TurnRequest, scope};
    use std::sync::Arc;

    #[tokio::test]
    async fn execution_is_requested() {
        let handle = TurnControl::new(AgentMode::Plan);
        let result = scope(
            Arc::clone(&handle),
            PlanExecuteTool::new().execute(json!({})),
        )
        .await
        .unwrap();
        assert!(result.success);
        let requests = {
            let mut state = match handle.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.take_requests()
        };
        assert!(matches!(
            requests[0],
            TurnRequest::SetMode(AgentMode::Execute)
        ));
    }

    #[tokio::test]
    async fn the_summary_is_reported() {
        let handle = TurnControl::new(AgentMode::Plan);
        let result = scope(
            Arc::clone(&handle),
            PlanExecuteTool::new().execute(json!({"summary": "rename the module"})),
        )
        .await
        .unwrap();
        assert!(result.output.as_str().contains("rename the module"), "{result:?}");
    }

    #[tokio::test]
    async fn outside_a_turn_the_tool_refuses() {
        let error = PlanExecuteTool::new().execute(json!({})).await.unwrap_err();
        assert!(
            error.to_string().contains("inside an active agent turn"),
            "{error}"
        );
    }
}
