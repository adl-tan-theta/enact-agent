//! `radar` — context budget attribution.
//!
//! Cost tells you what a turn cost in money. Context tells you what a turn
//! cost in the window, and that is the number that decides whether the next
//! tool call still fits. `radar` reports the per-source breakdown of the
//! context the last request carried: the system prompt, the memory preamble,
//! tool output, the conversation, and anything externally framed.

use async_trait::async_trait;
use serde_json::json;

use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

use crate::context_ledger::{self, ContextLedger};

/// Report the context budget of the recent turns.
pub struct RadarTool;

impl RadarTool {
    /// Build the tool. It reads process state, so it needs no policy.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RadarTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one ledger as a report.
fn render(ledger: &ContextLedger) -> String {
    let mut out = format!(
        "Turn {} — {} message(s), {} characters (~{} tokens)\n",
        ledger.turn_id,
        ledger.total_messages,
        ledger.total_chars,
        ledger.total_tokens()
    );
    if let Some(alias) = &ledger.agent_alias {
        out.push_str(&format!("Agent: {alias}\n"));
    }
    for bucket in &ledger.buckets {
        out.push_str(&format!(
            "  {:<18} {:>5} message(s)  {:>9} chars  {:>6.1}%  (~{} tokens)\n",
            bucket.source,
            bucket.messages,
            bucket.chars,
            ledger.share_of(&bucket.source),
            bucket.chars / 4
        ));
    }
    out
}

#[async_trait]
impl Tool for RadarTool {
    fn name(&self) -> &str {
        "radar"
    }

    fn description(&self) -> &str {
        "Report where the context window went. Shows the per-source breakdown of the last request — system prompt, memory preamble, tool output, conversation, externally framed content — with the share each took. Use it when a turn is running long or a tool result feels like it ate the window."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "turns": {
                    "type": "integer",
                    "description": "How many recent turns to report (default 1)."
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "json"],
                    "description": "Report format (default 'text')."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let turns = args
            .get("turns")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(1)
            .clamp(1, 16);
        let format = args
            .get("format")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("text");

        Ok(report(&context_ledger::ledgers(), turns, format))
    }
}

/// Build the report for the given ledgers.
///
/// The ledgers are a parameter rather than read from the process-wide store
/// so the report can be exercised without depending on what other turns have
/// recorded.
fn report(ledgers: &[ContextLedger], turns: usize, format: &str) -> ToolResult {
    if ledgers.is_empty() {
        return ToolResult {
            success: true,
            output: ToolOutput::text(
                "No turn has been accounted yet. The ledger fills in as turns run.\n",
            ),
            error: None,
        };
    }
    let selected: Vec<&ContextLedger> = ledgers.iter().take(turns).collect();

    if format == "json" {
        let payload: Vec<serde_json::Value> = selected
            .iter()
            .map(|ledger| {
                json!({
                    "turn_id": ledger.turn_id,
                    "agent_alias": ledger.agent_alias,
                    "total_messages": ledger.total_messages,
                    "total_chars": ledger.total_chars,
                    "total_tokens": ledger.total_tokens(),
                    "buckets": ledger.buckets.iter().map(|bucket| json!({
                        "source": bucket.source,
                        "messages": bucket.messages,
                        "chars": bucket.chars,
                        "share_percent": ledger.share_of(&bucket.source),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        return ToolResult {
            success: true,
            output: ToolOutput::json(serde_json::Value::Array(payload)),
            error: None,
        };
    }

    let mut out = String::new();
    for ledger in selected {
        out.push_str(&render(ledger));
        out.push('\n');
    }
    ToolResult {
        success: true,
        output: ToolOutput::text(out),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::model_provider::ChatMessage;

    #[test]
    fn an_empty_ledger_says_so() {
        let result = report(&[], 1, "text");
        assert!(result.success);
        assert!(result.output.as_str().contains("No turn"));
    }

    #[test]
    fn a_recorded_turn_is_reported() {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "system prompt".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "the question".to_string(),
            },
        ];
        let ledger = context_ledger::account("radar-test-turn", Some("main"), &messages, None);
        let result = report(&[ledger], 1, "text");
        assert!(result.success);
        assert!(result.output.as_str().contains("radar-test-turn"));
        assert!(result.output.as_str().contains("system_prompt"));
    }

    #[test]
    fn the_json_format_carries_the_breakdown() {
        let messages = vec![ChatMessage {
            role: "system".to_string(),
            content: "system prompt".to_string(),
        }];
        let ledger = context_ledger::account("t-json", Some("main"), &messages, None);
        let result = report(&[ledger], 1, "json");
        assert!(result.success);
        let payload = result.output.data().expect("the json payload");
        assert_eq!(payload[0]["turn_id"], json!("t-json"));
        assert_eq!(payload[0]["buckets"][0]["source"], json!("system_prompt"));
    }
}
