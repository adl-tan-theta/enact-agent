//! Declarative hook configuration.
//!
//! A declarative hook maps a lifecycle event to a command. The command
//! receives the event payload as JSON on stdin and answers with a JSON
//! decision on stdout. That is the whole contract: no plugin runtime, no
//! WASM host, no rebuild — a script is a hook.
//!
//! ```toml
//! [[hooks.commands]]
//! event = "before_tool_call"
//! matcher = "shell"
//! command = "bin/audit-shell.sh"
//! timeout_ms = 2000
//!
//! [[hooks.commands]]
//! event = "after_tool_call"
//! matcher = "file_read"
//! command = "bin/redact-secrets.sh"
//! fail_open = false
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The lifecycle event a declarative hook observes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Before a tool call is dispatched. May block it or rewrite its
    /// arguments.
    BeforeToolCall,
    /// After a tool call completed. May rewrite its output.
    AfterToolCall,
    /// Before the model is called. May block the call.
    BeforeLlmCall,
    /// Before the prompt is assembled. May rewrite the prompt.
    BeforePromptBuild,
    /// A session started. Observation only.
    SessionStart,
    /// A session ended. Observation only.
    SessionEnd,
}

impl HookEvent {
    /// The wire name of the event.
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::BeforeToolCall => "before_tool_call",
            HookEvent::AfterToolCall => "after_tool_call",
            HookEvent::BeforeLlmCall => "before_llm_call",
            HookEvent::BeforePromptBuild => "before_prompt_build",
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
        }
    }

    /// Every event name, for documentation and error messages.
    pub fn all() -> [HookEvent; 6] {
        [
            HookEvent::BeforeToolCall,
            HookEvent::AfterToolCall,
            HookEvent::BeforeLlmCall,
            HookEvent::BeforePromptBuild,
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
        ]
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The default timeout for a declarative hook command.
pub const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;

/// Whether a failing hook lets the operation continue.
fn default_fail_open() -> bool {
    true
}

/// One declarative hook rule: an event mapped to a command.
///
/// The command is run once per matching event. It receives the event payload
/// as a single JSON object on stdin and answers with a single JSON object on
/// stdout:
///
/// ```json
/// {"decision": "continue"}
/// {"decision": "block", "reason": "not allowed here"}
/// {"decision": "rewrite", "arguments": {"path": "safe.txt"}}
/// {"decision": "rewrite", "output": "redacted"}
/// ```
///
/// A command that exits non-zero, times out, or prints something that is not
/// a decision is a *failure*. A failing hook continues the operation when
/// `fail_open` is true (the default) and blocks it when it is false.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct HookCommandConfig {
    /// The event this hook observes.
    pub event: HookEvent,

    /// The command to run. It receives the event payload as JSON on stdin.
    pub command: String,

    /// Only fire for tools whose name matches this glob. Tool events only.
    #[serde(default)]
    pub matcher: Option<String>,

    /// How long the command may run before it is killed.
    #[serde(default)]
    pub timeout_ms: Option<u64>,

    /// When the command fails or times out, continue (true, the default) or
    /// block the operation (false).
    #[serde(default = "default_fail_open")]
    pub fail_open: bool,

    /// Extra environment entries for the command.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Ordering among declarative hooks. Higher runs first.
    #[serde(default)]
    pub priority: i32,
}

impl HookCommandConfig {
    /// The effective timeout for this hook.
    pub fn effective_timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_HOOK_TIMEOUT_MS)
    }
}

/// The declarative hook list renders as an object array in the config
/// surfaces: each entry is a table with an `event`, a `command`, and the
/// optional matcher / timeout / fail-open knobs.
impl crate::traits::HasPropKind for Vec<HookCommandConfig> {
    const PROP_KIND: crate::traits::PropKind = crate::traits::PropKind::ObjectArray;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip_through_their_wire_names() {
        for event in HookEvent::all() {
            let json = serde_json::to_string(&event).unwrap();
            let parsed: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, parsed);
            assert_eq!(json, format!("\"{}\"", event.as_str()));
        }
    }

    #[test]
    fn a_rule_parses_from_toml_shape() {
        let rule: HookCommandConfig = serde_json::from_value(serde_json::json!({
            "event": "before_tool_call",
            "command": "bin/audit.sh",
            "matcher": "shell",
            "timeout_ms": 2500,
            "fail_open": false,
        }))
        .unwrap();
        assert_eq!(rule.event, HookEvent::BeforeToolCall);
        assert_eq!(rule.command, "bin/audit.sh");
        assert_eq!(rule.matcher.as_deref(), Some("shell"));
        assert_eq!(rule.effective_timeout_ms(), 2500);
        assert!(!rule.fail_open);
    }

    #[test]
    fn fail_open_defaults_to_true() {
        let rule: HookCommandConfig = serde_json::from_value(serde_json::json!({
            "event": "after_tool_call",
            "command": "bin/redact.sh",
        }))
        .unwrap();
        assert!(rule.fail_open);
        assert_eq!(rule.effective_timeout_ms(), DEFAULT_HOOK_TIMEOUT_MS);
    }

    #[test]
    fn a_rule_parses_from_toml() {
        let rule: HookCommandConfig = toml::from_str(
            r#"
            event = "after_tool_call"
            command = "bin/redact.sh"
            matcher = "file_*"
            timeout_ms = 1500
            fail_open = false
            priority = 5

            [env]
            ROLE = "audit"
            "#,
        )
        .unwrap();
        assert_eq!(rule.event, HookEvent::AfterToolCall);
        assert_eq!(rule.matcher.as_deref(), Some("file_*"));
        assert_eq!(rule.priority, 5);
        assert_eq!(rule.env.get("ROLE").map(String::as_str), Some("audit"));
    }

    #[test]
    fn a_rule_list_parses_from_a_toml_array_of_tables() {
        #[derive(Debug, Deserialize)]
        struct Holder {
            #[serde(default)]
            commands: Vec<HookCommandConfig>,
        }
        let holder: Holder = toml::from_str(
            r#"
            [[commands]]
            event = "before_tool_call"
            command = "bin/audit.sh"

            [[commands]]
            event = "session_start"
            command = "bin/hello.sh"
            "#,
        )
        .unwrap();
        assert_eq!(holder.commands.len(), 2);
        assert_eq!(holder.commands[0].event, HookEvent::BeforeToolCall);
        assert_eq!(holder.commands[1].event, HookEvent::SessionStart);
    }

    #[test]
    fn the_hook_list_is_an_object_array_in_the_config_surfaces() {
        assert_eq!(
            <Vec<HookCommandConfig> as crate::traits::HasPropKind>::PROP_KIND,
            crate::traits::PropKind::ObjectArray
        );
    }
}
