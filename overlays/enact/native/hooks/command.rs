//! Declarative command hooks.
//!
//! A configured hook runs a command, writes the event payload to its stdin,
//! and reads a JSON decision from its stdout. That is the entire contract —
//! a script is a hook, so adding one needs no plugin runtime, no WASM host
//! and no rebuild.
//!
//! The decision vocabulary:
//!
//! | decision | effect |
//! |---|---|
//! | `continue` | nothing changes |
//! | `block` | the operation is cancelled with the given reason |
//! | `rewrite` | the arguments or the output are replaced |
//!
//! A command that cannot be run, exits non-zero without printing a decision,
//! or prints something that is not a decision is a *failure*. A failing hook
//! continues the operation when `fail_open` is true (the default) and blocks
//! it when it is false — the same discipline the in-process hook runner uses
//! for a panicking handler.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use zeroclaw_api::hook::{HookHandler, HookResult, LlmCallHookContext};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::tool::ToolResult;
use zeroclaw_config::hooks::{HookCommandConfig, HookEvent};

/// What a hook command answered.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum HookDecision {
    /// Nothing changes.
    Continue,
    /// The operation is cancelled.
    Block {
        /// Why it was blocked. Shown to the model and the operator.
        #[serde(default)]
        reason: Option<String>,
    },
    /// The arguments or the output are replaced.
    Rewrite {
        /// Replacement arguments, for `before_tool_call`.
        #[serde(default)]
        arguments: Option<Value>,
        /// Replacement output, for `after_tool_call`.
        #[serde(default)]
        output: Option<String>,
    },
}

/// A hook backed by a configured command.
#[derive(Debug, Clone)]
pub struct CommandHook {
    config: HookCommandConfig,
}

impl CommandHook {
    /// Build a hook from its configuration.
    pub fn new(config: HookCommandConfig) -> Self {
        Self { config }
    }

    /// The rule this hook runs.
    pub fn config(&self) -> &HookCommandConfig {
        &self.config
    }

    /// Whether this hook fires for the named tool.
    fn matches_tool(&self, tool: &str) -> bool {
        match self.config.matcher.as_deref() {
            None => true,
            Some(pattern) => {
                pattern == tool
                    || (pattern.contains('*')
                        && glob_match(pattern, tool))
            }
        }
    }

    /// Run the command and read its decision.
    async fn decide(&self, payload: &Value) -> anyhow::Result<HookDecision> {
        let timeout = Duration::from_millis(self.config.effective_timeout_ms());
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.arg("/C").arg(&self.config.command);
            command
        } else {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(&self.config.command);
            command
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &self.config.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|error| anyhow::anyhow!("failed to run the hook command: {error}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let body = serde_json::to_vec(payload)?;
            stdin.write_all(&body).await?;
            stdin.write_all(b"\n").await?;
            let _ = stdin.shutdown().await;
        }

        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| anyhow::anyhow!("the hook command timed out"))?
            .map_err(|error| anyhow::anyhow!("the hook command failed: {error}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            // A parseable decision is authoritative even when the command
            // exits non-zero: the decision is the answer, the exit code is
            // the command's own business.
            if let Ok(decision) = serde_json::from_str::<HookDecision>(trimmed) {
                return Ok(decision);
            }
        }
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "the hook command exited with {} and printed no decision{}",
                output.status,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            );
        }
        Ok(HookDecision::Continue)
    }

    /// Turn a hook failure into the configured behaviour.
    fn on_failure(&self, error: anyhow::Error) -> HookDecision {
        if self.config.fail_open {
            HookDecision::Continue
        } else {
            HookDecision::Block {
                reason: Some(format!("hook {} failed: {error:#}", self.config.command)),
            }
        }
    }

    /// The reason a block decision carries.
    fn block_reason(&self, reason: Option<String>) -> String {
        reason.unwrap_or_else(|| format!("blocked by hook {}", self.config.command))
    }
}

/// A minimal glob matcher for `*` and `?`.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn helper(pattern: &[u8], text: &[u8]) -> bool {
        if pattern.is_empty() {
            return text.is_empty();
        }
        match pattern[0] {
            b'*' => {
                if helper(&pattern[1..], text) {
                    return true;
                }
                text.iter().enumerate().any(|(index, _)| helper(&pattern[1..], &text[index + 1..]))
            }
            b'?' => !text.is_empty() && helper(&pattern[1..], &text[1..]),
            literal => !text.is_empty() && text[0] == literal && helper(&pattern[1..], &text[1..]),
        }
    }
    helper(pattern.as_bytes(), text.as_bytes())
}

#[async_trait]
impl HookHandler for CommandHook {
    fn name(&self) -> &str {
        &self.config.command
    }

    fn priority(&self) -> i32 {
        self.config.priority
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        if self.config.event != HookEvent::BeforeToolCall || !self.matches_tool(&name) {
            return HookResult::Continue((name, args));
        }
        let payload = json!({
            "event": HookEvent::BeforeToolCall.as_str(),
            "tool": name,
            "arguments": args,
        });
        match self.decide(&payload).await {
            Ok(HookDecision::Continue) => HookResult::Continue((name, args)),
            Ok(HookDecision::Block { reason }) => HookResult::Cancel(self.block_reason(reason)),
            Ok(HookDecision::Rewrite { arguments, .. }) => {
                HookResult::Continue((name, arguments.unwrap_or(args)))
            }
            Err(error) => match self.on_failure(error) {
                HookDecision::Block { reason } => HookResult::Cancel(self.block_reason(reason)),
                _ => HookResult::Continue((name, args)),
            },
        }
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        if self.config.event != HookEvent::BeforePromptBuild {
            return HookResult::Continue(prompt);
        }
        let payload = json!({
            "event": HookEvent::BeforePromptBuild.as_str(),
            "prompt": prompt,
        });
        match self.decide(&payload).await {
            Ok(HookDecision::Continue) => HookResult::Continue(prompt),
            Ok(HookDecision::Block { reason }) => HookResult::Cancel(self.block_reason(reason)),
            Ok(HookDecision::Rewrite { output, .. }) => {
                HookResult::Continue(output.unwrap_or(prompt))
            }
            Err(error) => match self.on_failure(error) {
                HookDecision::Block { reason } => HookResult::Cancel(self.block_reason(reason)),
                _ => HookResult::Continue(prompt),
            },
        }
    }

    async fn before_llm_call(
        &self,
        messages: &mut Vec<ChatMessage>,
        model: &mut String,
    ) -> HookResult<()> {
        self.before_llm_call_with_context(
            &LlmCallHookContext::default(),
            messages,
            model,
        )
        .await
    }

    async fn before_llm_call_with_context(
        &self,
        context: &LlmCallHookContext,
        messages: &mut Vec<ChatMessage>,
        model: &mut String,
    ) -> HookResult<()> {
        if self.config.event != HookEvent::BeforeLlmCall {
            return HookResult::Continue(());
        }
        // The trust split is recomputed against the messages as they are now,
        // not against the indices the runner captured: an earlier handler may
        // have inserted or removed a message, and a stale index would rewrite
        // the wrong half of the request.
        let trust = crate::hooks::trust::classify_messages(messages, Some(context.turn_trust));
        let untrusted_content: Vec<String> = messages
            .iter()
            .enumerate()
            .filter(|(index, _)| trust.is_untrusted(*index))
            .map(|(_, message)| message.content.clone())
            .collect();
        let payload = json!({
            "event": HookEvent::BeforeLlmCall.as_str(),
            "model": model,
            "message_count": messages.len(),
            "turn_trust": match context.turn_trust {
                zeroclaw_api::ingress::TrustClass::Trusted => "trusted",
                zeroclaw_api::ingress::TrustClass::Untrusted => "untrusted",
            },
            "untrusted_indices": trust.untrusted_indices(),
            "untrusted_content": untrusted_content.join("\n\n"),
        });
        match self.decide(&payload).await {
            Ok(HookDecision::Continue) => HookResult::Continue(()),
            Ok(HookDecision::Block { reason }) => HookResult::Cancel(self.block_reason(reason)),
            Ok(HookDecision::Rewrite { output, .. }) => {
                // A rewrite replaces the *untrusted* half only. The operator's
                // own instructions are never touched by a declarative hook.
                if let Some(output) = output {
                    for (index, message) in messages.iter_mut().enumerate() {
                        if trust.is_untrusted(index) {
                            message.content = output.clone();
                        }
                    }
                }
                HookResult::Continue(())
            }
            Err(error) => match self.on_failure(error) {
                HookDecision::Block { reason } => HookResult::Cancel(self.block_reason(reason)),
                _ => HookResult::Continue(()),
            },
        }
    }

    async fn after_tool_call(&self, tool: &str, result: &ToolResult) -> HookResult<ToolResult> {
        if self.config.event != HookEvent::AfterToolCall || !self.matches_tool(tool) {
            return HookResult::Continue(result.clone());
        }
        let payload = json!({
            "event": HookEvent::AfterToolCall.as_str(),
            "tool": tool,
            "success": result.success,
            "output": result.output.as_str(),
        });
        match self.decide(&payload).await {
            Ok(HookDecision::Continue) => HookResult::Continue(result.clone()),
            Ok(HookDecision::Block { reason }) => HookResult::Cancel(self.block_reason(reason)),
            Ok(HookDecision::Rewrite { output, .. }) => {
                let mut rewritten = result.clone();
                if let Some(output) = output {
                    // Rewrite the display text in place so the tool's declared
                    // attachments survive; only the text the model reads
                    // changes.
                    rewritten.output.map_text(|_| output);
                }
                HookResult::Continue(rewritten)
            }
            Err(error) => match self.on_failure(error) {
                HookDecision::Block { reason } => HookResult::Cancel(self.block_reason(reason)),
                _ => HookResult::Continue(result.clone()),
            },
        }
    }

    async fn on_session_start(&self, session_id: &str, channel: &str) {
        if self.config.event != HookEvent::SessionStart {
            return;
        }
        let payload = json!({
            "event": HookEvent::SessionStart.as_str(),
            "session_id": session_id,
            "channel": channel,
        });
        let _ = self.decide(&payload).await;
    }

    async fn on_session_end(&self, session_id: &str, channel: &str) {
        if self.config.event != HookEvent::SessionEnd {
            return;
        }
        let payload = json!({
            "event": HookEvent::SessionEnd.as_str(),
            "session_id": session_id,
            "channel": channel,
        });
        let _ = self.decide(&payload).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn rule(event: HookEvent, command: &str) -> HookCommandConfig {
        HookCommandConfig {
            event,
            command: command.to_string(),
            matcher: None,
            timeout_ms: Some(5_000),
            fail_open: true,
            env: BTreeMap::new(),
            priority: 0,
        }
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("shell*", "shell"));
        assert!(glob_match("*_read", "file_read"));
        assert!(glob_match("sh?ll", "shell"));
        assert!(!glob_match("shell", "shellx"));
        assert!(!glob_match("read", "file_read"));
    }

    #[test]
    fn a_matcher_limits_the_tools_the_hook_fires_for() {
        let hook = CommandHook::new(HookCommandConfig {
            matcher: Some("file_*".to_string()),
            ..rule(HookEvent::BeforeToolCall, "true")
        });
        assert!(hook.matches_tool("file_read"));
        assert!(!hook.matches_tool("shell"));
    }

    #[tokio::test]
    async fn a_continue_decision_leaves_the_call_alone() {
        let hook = CommandHook::new(rule(
            HookEvent::BeforeToolCall,
            "cat > /dev/null; echo '{\"decision\":\"continue\"}'",
        ));
        let outcome = hook
            .before_tool_call("shell".to_string(), json!({"command": "ls"}))
            .await;
        match outcome {
            HookResult::Continue((name, args)) => {
                assert_eq!(name, "shell");
                assert_eq!(args["command"], json!("ls"));
            }
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }

    #[tokio::test]
    async fn a_block_decision_cancels_the_call() {
        let hook = CommandHook::new(rule(
            HookEvent::BeforeToolCall,
            "echo '{\"decision\":\"block\",\"reason\":\"not here\"}'",
        ));
        let outcome = hook
            .before_tool_call("shell".to_string(), json!({}))
            .await;
        match outcome {
            HookResult::Cancel(reason) => assert_eq!(reason, "not here"),
            HookResult::Continue(_) => panic!("expected a cancel"),
        }
    }

    #[tokio::test]
    async fn a_rewrite_decision_replaces_the_arguments() {
        let hook = CommandHook::new(rule(
            HookEvent::BeforeToolCall,
            "echo '{\"decision\":\"rewrite\",\"arguments\":{\"path\":\"safe.txt\"}}'",
        ));
        let outcome = hook
            .before_tool_call("file_read".to_string(), json!({"path": "secret.txt"}))
            .await;
        match outcome {
            HookResult::Continue((_, args)) => assert_eq!(args["path"], json!("safe.txt")),
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }

    #[tokio::test]
    async fn the_payload_reaches_the_command_on_stdin() {
        let hook = CommandHook::new(rule(
            HookEvent::BeforeToolCall,
            "read line; echo \"{\\\"decision\\\":\\\"rewrite\\\",\\\"arguments\\\":$line}\"",
        ));
        let outcome = hook
            .before_tool_call("shell".to_string(), json!({"command": "ls"}))
            .await;
        match outcome {
            HookResult::Continue((_, args)) => {
                assert_eq!(args["tool"], json!("shell"));
                assert_eq!(args["arguments"]["command"], json!("ls"));
            }
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }

    #[tokio::test]
    async fn a_failing_command_continues_when_fail_open() {
        let hook = CommandHook::new(rule(HookEvent::BeforeToolCall, "exit 3"));
        let outcome = hook
            .before_tool_call("shell".to_string(), json!({}))
            .await;
        assert!(matches!(outcome, HookResult::Continue(_)));
    }

    #[tokio::test]
    async fn a_failing_command_blocks_when_fail_closed() {
        let hook = CommandHook::new(HookCommandConfig {
            fail_open: false,
            ..rule(HookEvent::BeforeToolCall, "exit 3")
        });
        let outcome = hook
            .before_tool_call("shell".to_string(), json!({}))
            .await;
        match outcome {
            HookResult::Cancel(reason) => assert!(reason.contains("failed"), "{reason}"),
            HookResult::Continue(_) => panic!("expected a cancel"),
        }
    }

    #[tokio::test]
    async fn a_hook_for_another_event_does_not_fire() {
        let hook = CommandHook::new(rule(
            HookEvent::AfterToolCall,
            "echo '{\"decision\":\"block\"}'",
        ));
        let outcome = hook
            .before_tool_call("shell".to_string(), json!({}))
            .await;
        assert!(matches!(outcome, HookResult::Continue(_)));
    }

    #[tokio::test]
    async fn a_timing_out_command_is_a_failure() {
        let hook = CommandHook::new(HookCommandConfig {
            timeout_ms: Some(50),
            ..rule(HookEvent::BeforeToolCall, "sleep 5")
        });
        let outcome = hook
            .before_tool_call("shell".to_string(), json!({}))
            .await;
        assert!(matches!(outcome, HookResult::Continue(_)));
    }

    #[tokio::test]
    async fn an_after_call_rewrite_replaces_the_output() {
        let hook = CommandHook::new(rule(
            HookEvent::AfterToolCall,
            "echo '{\"decision\":\"rewrite\",\"output\":\"redacted\"}'",
        ));
        let result = ToolResult::ok("the secret token is hunter2");
        let outcome = hook.after_tool_call("file_read", &result).await;
        match outcome {
            HookResult::Continue(rewritten) => {
                assert_eq!(rewritten.output.as_str(), "redacted");
            }
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }

    #[tokio::test]
    async fn an_after_call_rewrite_keeps_the_tool_attachments() {
        let hook = CommandHook::new(rule(
            HookEvent::AfterToolCall,
            "echo '{\"decision\":\"rewrite\",\"output\":\"see the attached chart\"}'",
        ));
        let mut result = ToolResult::ok("the raw text");
        result.output = result.output.clone().with_attachment(
            zeroclaw_api::media::RenderedMarker {
                target: "chart.png".to_string(),
                kind: zeroclaw_api::media::MarkerKind::Image,
            },
        );
        let outcome = hook.after_tool_call("file_read", &result).await;
        match outcome {
            HookResult::Continue(rewritten) => {
                assert_eq!(rewritten.output.as_str(), "see the attached chart");
                assert_eq!(rewritten.output.attachments().len(), 1);
            }
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }

    #[tokio::test]
    async fn an_after_call_block_cancels_the_result() {
        let hook = CommandHook::new(rule(
            HookEvent::AfterToolCall,
            "echo '{\"decision\":\"block\",\"reason\":\"not allowed\"}'",
        ));
        let outcome = hook.after_tool_call("file_read", &ToolResult::ok("x")).await;
        match outcome {
            HookResult::Cancel(reason) => assert_eq!(reason, "not allowed"),
            HookResult::Continue(_) => panic!("expected a cancel"),
        }
    }

    #[tokio::test]
    async fn a_before_call_hook_does_not_fire_on_the_after_call_path() {
        let hook = CommandHook::new(rule(
            HookEvent::BeforeToolCall,
            "echo '{\"decision\":\"block\"}'",
        ));
        let outcome = hook
            .after_tool_call("file_read", &ToolResult::ok("x"))
            .await;
        assert!(matches!(outcome, HookResult::Continue(_)));
    }

    // ── The config → runner → command wiring ───────────────────────────

    fn hooks_config(commands: Vec<HookCommandConfig>) -> zeroclaw_config::schema::HooksConfig {
        zeroclaw_config::schema::HooksConfig {
            enabled: true,
            builtin: zeroclaw_config::schema::BuiltinHooksConfig {
                command_logger: false,
                webhook_audit: Default::default(),
            },
            commands,
        }
    }

    #[tokio::test]
    async fn a_configured_command_hook_rewrites_the_tool_output() {
        let runner = super::super::runner::HookRunner::from_config(&hooks_config(vec![rule(
            HookEvent::AfterToolCall,
            "echo '{\"decision\":\"rewrite\",\"output\":\"redacted\"}'",
        )]));

        let outcome = runner
            .run_after_tool_call("file_read", &ToolResult::ok("token is hunter2"))
            .await;

        match outcome {
            HookResult::Continue(rewritten) => {
                assert_eq!(rewritten.output.as_str(), "redacted");
            }
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }

    #[tokio::test]
    async fn a_configured_command_hook_can_block_a_tool_call() {
        let runner = super::super::runner::HookRunner::from_config(&hooks_config(vec![rule(
            HookEvent::BeforeToolCall,
            "echo '{\"decision\":\"block\",\"reason\":\"denied by policy\"}'",
        )]));

        let outcome = runner
            .run_before_tool_call("shell".to_string(), json!({}))
            .await;

        match outcome {
            HookResult::Cancel(reason) => assert_eq!(reason, "denied by policy"),
            HookResult::Continue(_) => panic!("expected a cancel"),
        }
    }

    #[tokio::test]
    async fn a_configured_command_hook_honours_its_matcher() {
        let runner = super::super::runner::HookRunner::from_config(&hooks_config(vec![
            HookCommandConfig {
                matcher: Some("shell".to_string()),
                ..rule(
                    HookEvent::BeforeToolCall,
                    "echo '{\"decision\":\"block\",\"reason\":\"not here\"}'",
                )
            },
        ]));

        let outcome = runner
            .run_before_tool_call("file_read".to_string(), json!({}))
            .await;

        assert!(
            matches!(outcome, HookResult::Continue(_)),
            "a matcher that does not match must leave the call alone"
        );
    }

    #[tokio::test]
    async fn a_configured_command_hook_that_fails_does_not_block_by_default() {
        let runner = super::super::runner::HookRunner::from_config(&hooks_config(vec![rule(
            HookEvent::BeforeToolCall,
            "exit 7",
        )]));

        let outcome = runner
            .run_before_tool_call("shell".to_string(), json!({}))
            .await;

        assert!(
            matches!(outcome, HookResult::Continue(_)),
            "a failing hook with fail_open must let the call through"
        );
    }

    #[tokio::test]
    async fn a_configured_command_hook_that_fails_blocks_when_closed() {
        let runner = super::super::runner::HookRunner::from_config(&hooks_config(vec![
            HookCommandConfig {
                fail_open: false,
                ..rule(HookEvent::BeforeToolCall, "exit 7")
            },
        ]));

        let outcome = runner
            .run_before_tool_call("shell".to_string(), json!({}))
            .await;

        assert!(
            outcome.is_cancel(),
            "a failing hook with fail_open = false must block the call"
        );
    }

    #[tokio::test]
    async fn a_runner_without_configured_commands_leaves_the_result_alone() {
        let runner = super::super::runner::HookRunner::from_config(&hooks_config(Vec::new()));

        let outcome = runner
            .run_after_tool_call("file_read", &ToolResult::ok("unchanged"))
            .await;

        match outcome {
            HookResult::Continue(unchanged) => {
                assert_eq!(unchanged.output.as_str(), "unchanged");
            }
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }
}
