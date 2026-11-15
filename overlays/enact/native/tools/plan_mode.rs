//! `plan_mode` — put the turn into plan mode, or report which mode it is in.
//!
//! Plan mode is the posture a turn takes when it may look but not act. The
//! read-only tools still run; the write and execute tools are refused with a
//! message that points back here. It is the mode an operator asks for when
//! they want the plan before the change.
//!
//! Leaving plan mode is a separate decision, and it is the operator's: the
//! agent asks with `plan_execute`, which the approval policy judges like any
//! other write.

use async_trait::async_trait;
use serde_json::{Value, json};

use zeroclaw_api::tool::{Tool, ToolResult};

/// The mode the turn is in, and the way into plan mode.
pub struct PlanModeTool;

impl PlanModeTool {
    /// Build the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for PlanModeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PlanModeTool {
    fn name(&self) -> &str {
        "plan_mode"
    }

    fn description(&self) -> &str {
        "Report the turn's mode, or put the turn into plan mode. In plan mode the agent may read and investigate but not change anything: write and execute tools are refused until the operator allows execution. Use action \"plan\" to enter plan mode, \"status\" to report which mode the turn is in."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "plan"],
                    "description": "status: report the current mode. plan: put the turn into plan mode, where the agent may read but not change anything."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("status")
            .to_ascii_lowercase();

        let control = crate::agent::turn_control::current()?;
        let current = crate::agent::turn_control::current_mode();

        match action.as_str() {
            "status" => Ok(ToolResult::ok(format!(
                "The turn is in `{}` mode.{}",
                current.as_str(),
                if current == crate::agent::turn_control::AgentMode::Plan {
                    " Write and execute tools are refused; ask to act with plan_execute."
                } else {
                    ""
                }
            ))),
            "plan" => {
                {
                    let mut state = match control.lock() {
                        Ok(state) => state,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    state.request_mode(crate::agent::turn_control::AgentMode::Plan);
                }
                Ok(ToolResult::ok(
                    "Plan mode requested. From the next step on, write and execute tools are refused: investigate and write the plan down with TodoWrite, then ask to act with plan_execute.",
                ))
            }
            other => Ok(ToolResult::err(format!(
                "Unknown action `{other}` for the plan_mode tool; expected one of: status, plan."
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::turn_control::{AgentMode, TurnControl, TurnRequest, scope};
    use std::sync::Arc;

    #[tokio::test]
    async fn the_status_reports_the_current_mode() {
        let handle = TurnControl::new(AgentMode::Plan);
        let result = scope(
            Arc::clone(&handle),
            PlanModeTool::new().execute(json!({"action": "status"})),
        )
        .await
        .unwrap();
        assert!(result.success);
        assert!(result.output.as_str().contains("plan"), "{result:?}");
    }

    #[tokio::test]
    async fn plan_mode_is_requested() {
        let handle = TurnControl::new(AgentMode::Execute);
        let result = scope(
            Arc::clone(&handle),
            PlanModeTool::new().execute(json!({"action": "plan"})),
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
        assert!(matches!(requests[0], TurnRequest::SetMode(AgentMode::Plan)));
    }

    #[tokio::test]
    async fn an_unknown_action_is_refused() {
        let handle = TurnControl::new(AgentMode::Execute);
        let result = scope(
            Arc::clone(&handle),
            PlanModeTool::new().execute(json!({"action": "nope"})),
        )
        .await
        .unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or_default().contains("status, plan"),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn outside_a_turn_the_tool_refuses() {
        let error = PlanModeTool::new()
            .execute(json!({"action": "status"}))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("inside an active agent turn"),
            "{error}"
        );
    }
}
