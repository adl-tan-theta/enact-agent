//! Approval tiers: read, write, exec.
//!
//! The runtime already decides whether a tool call needs an operator, but the
//! decision is per tool *name*: `always_ask` lists the tools that prompt, and
//! `auto_approve` lists the ones that never do. That answers "does this tool
//! prompt?" but not "how much does this tool do?".
//!
//! A tier answers the second question, and it is what an operator wants to
//! reason about. A tool that only reads is a different risk from one that
//! executes, even when both are on the same allowlist. The tier is a
//! classification; the approval decision still comes from the operator's
//! policy, so a tier never widens what the policy already allows.
//!
//! An operator may name a tier in `always_ask` or `auto_approve` —
//! `auto_approve = ["read"]` covers every reader, today and tomorrow,
//! without a list to maintain.

use crate::tools::families;

/// How much a tool does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApprovalTier {
    /// The tool observes. It reads state and reports it.
    Read,
    /// The tool changes state in the workspace.
    Write,
    /// The tool runs something outside the workspace's own files.
    Exec,
}

impl ApprovalTier {
    /// The wire name of the tier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalTier::Read => "read",
            ApprovalTier::Write => "write",
            ApprovalTier::Exec => "exec",
        }
    }

    /// Parse a tier from its wire name.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "read" => Some(ApprovalTier::Read),
            "write" => Some(ApprovalTier::Write),
            "exec" => Some(ApprovalTier::Exec),
            _ => None,
        }
    }

    /// Whether the tier changes state.
    #[must_use]
    pub fn is_mutating(self) -> bool {
        !matches!(self, ApprovalTier::Read)
    }
}

/// The tools each tier covers, by name.
///
/// The list is deliberately conservative: a tool that is not named is treated
/// as `Write`, not as `Read`. A tool the runtime does not know about may well
/// run a command, and defaulting it to the safer middle tier means an
/// operator's `always_ask` list still catches it.
///
/// A tool that mixes reading and writing — `git_operations` can list a
/// branch or push one — is not named here either, so it lands in `Write`.
const READ_TOOLS: &[&str] = &[
    "file_read",
    "glob_search",
    "content_search",
    "web_fetch",
    "web_search_tool",
    "tool_search",
    "memory_recall",
    "read_skill",
    "sessions_current",
    "sessions_list",
    "sessions_history",
    "mcp_resources",
    "mcp_prompts",
    "email_search",
    "email_read",
    "browser_open",
    "text_browser",
    "screenshot",
    "image_info",
    "calculator",
    "weather",
    "poll",
    "cloud_ops",
    "cloud_patterns",
    "ask_user",
    "escalate_to_human",
    "channel_room",
    "radar",
    "compose",
    "expand",
    "ast_grep",
    "callgraph",
    "graph",
    "wait",
    // The turn's control channel. `plan_mode` reports the mode and enters the
    // restrictive one, and `checkpoint` only marks a point in the turn's own
    // conversation: neither changes the machine, so neither needs an
    // operator's approval to run.
    "plan_mode",
    "checkpoint",
];

/// The tools that run something.
const EXEC_TOOLS: &[&str] = &[
    "shell",
    "schedule",
    "cron_add",
    "cron_update",
    "cron_run",
    "cron_run_shell",
    "delegate",
    "spawn_subagent",
    "send_message_to_peer",
    "a2a_send",
    "http_request",
    "skill_http",
    "sandbox",
    "browser",
    "pushover",
];

/// The tier a tool belongs to.
#[must_use]
pub fn tier_for(tool_name: &str) -> ApprovalTier {
    if EXEC_TOOLS.contains(&tool_name) {
        return ApprovalTier::Exec;
    }
    if READ_TOOLS.contains(&tool_name) {
        return ApprovalTier::Read;
    }
    // Unknown tools are treated as writes: they may touch the workspace, and
    // a write is what an operator's policy usually wants to see.
    ApprovalTier::Write
}

/// The tier of a call.
///
/// A consolidated family is judged by the member its action names: `memory`
/// with the `recall` action reads, and `memory` with the `store` action
/// writes. Judging the family as a whole would put every operation in the
/// most careful tier, and the operator's policy would stop matching what the
/// call actually does.
#[must_use]
pub fn tier_for_call(tool_name: &str, action: Option<&str>) -> ApprovalTier {
    match families::resolve(tool_name, action) {
        Some((member, _)) => tier_for(member),
        None => tier_for(tool_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_is_read_tier() {
        assert_eq!(tier_for("file_read"), ApprovalTier::Read);
        assert_eq!(tier_for("memory_recall"), ApprovalTier::Read);
    }

    #[test]
    fn a_writer_is_write_tier() {
        assert_eq!(tier_for("file_write"), ApprovalTier::Write);
        assert_eq!(tier_for("file_edit"), ApprovalTier::Write);
        // A tool that can both read and write is not a reader.
        assert_eq!(tier_for("git_operations"), ApprovalTier::Write);
        assert_eq!(tier_for("knowledge"), ApprovalTier::Write);
    }

    #[test]
    fn an_executor_is_exec_tier() {
        assert_eq!(tier_for("shell"), ApprovalTier::Exec);
        assert_eq!(tier_for("schedule"), ApprovalTier::Exec);
        assert_eq!(tier_for("browser"), ApprovalTier::Exec);
    }

    #[test]
    fn an_unknown_tool_is_write_tier() {
        // Not `read`: a tool the runtime does not know may well write.
        assert_eq!(tier_for("some_tool_from_the_future"), ApprovalTier::Write);
    }

    #[test]
    fn a_tier_round_trips_through_its_wire_name() {
        for tier in [
            ApprovalTier::Read,
            ApprovalTier::Write,
            ApprovalTier::Exec,
        ] {
            assert_eq!(ApprovalTier::parse(tier.as_str()), Some(tier));
        }
    }

    #[test]
    fn an_unknown_tier_name_is_refused() {
        assert_eq!(ApprovalTier::parse("sideways"), None);
    }

    #[test]
    fn only_read_tier_is_immutable() {
        assert!(!ApprovalTier::Read.is_mutating());
        assert!(ApprovalTier::Write.is_mutating());
        assert!(ApprovalTier::Exec.is_mutating());
    }

    #[test]
    fn a_family_call_is_judged_by_its_member() {
        // The memory family reads with `recall` and writes with `store`.
        assert_eq!(
            tier_for_call("memory", Some("recall")),
            ApprovalTier::Read,
            "a recall reads"
        );
        assert_eq!(
            tier_for_call("memory", Some("store")),
            ApprovalTier::Write,
            "a store writes"
        );
        // The schedule family always runs something.
        assert_eq!(tier_for_call("cron", Some("schedule_list")), ApprovalTier::Exec);
    }

    #[test]
    fn a_call_without_an_action_falls_back_to_the_tool() {
        assert_eq!(tier_for_call("file_read", None), ApprovalTier::Read);
        assert_eq!(tier_for_call("memory", None), ApprovalTier::Write);
    }
}
