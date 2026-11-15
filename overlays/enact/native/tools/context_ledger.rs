//! The context ledger: where the context window actually goes.
//!
//! The runtime tracks what a turn costs in money. It does not track what a
//! turn costs in *context*, which is the number that decides whether the next
//! tool call still fits. A turn can be cheap and still drown: a hundred
//! kilobytes of tool output, a memory preamble, and a system prompt all
//! compete for the same window.
//!
//! The ledger records, per turn, how many messages and characters each
//! source contributed to the request that was actually sent. `radar` reads it.

use std::sync::{Mutex, OnceLock};

use zeroclaw_api::model_provider::ChatMessage;

use crate::tools::workspace_scan::estimate_tokens;

/// How many turns the ledger keeps.
pub const LEDGER_CAPACITY: usize = 16;

/// One source of context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBucket {
    /// Where the content came from: `system_prompt`, `memory`, `tool_output`,
    /// `assistant_history`, `user_input`, or `external_content`.
    pub source: String,
    /// How many messages this source contributed.
    pub messages: usize,
    /// How many characters this source contributed.
    pub chars: usize,
}

/// One turn's context accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLedger {
    /// The turn this ledger describes.
    pub turn_id: String,
    /// The agent the turn ran under, when it is known.
    pub agent_alias: Option<String>,
    /// The per-source breakdown, largest first.
    pub buckets: Vec<ContextBucket>,
    /// The total number of messages in the request.
    pub total_messages: usize,
    /// The total number of characters in the request.
    pub total_chars: usize,
}

impl ContextLedger {
    /// The total number of tokens the request is worth, approximately.
    #[must_use]
    pub fn total_tokens(&self) -> usize {
        estimate_tokens(&self.total_chars.to_string())
    }

    /// The share of the context one source took, as a percentage.
    #[must_use]
    pub fn share_of(&self, source: &str) -> f64 {
        if self.total_chars == 0 {
            return 0.0;
        }
        self.buckets
            .iter()
            .filter(|bucket| bucket.source == source)
            .map(|bucket| bucket.chars as f64)
            .sum::<f64>()
            / self.total_chars as f64
            * 100.0
    }
}

/// The marker that frames externally-authored content.
const EXTERNAL_UNTRUSTED_MARKER: &str = "<<<EXTERNAL_UNTRUSTED_CONTENT";

/// The bucket a message belongs to.
fn bucket_for(role: &str, content: &str, memory_index: Option<usize>, index: usize) -> &'static str {
    if content.contains(EXTERNAL_UNTRUSTED_MARKER) {
        return "external_content";
    }
    match role {
        "system" => "system_prompt",
        "tool" => "tool_output",
        "assistant" => "assistant_history",
        "user" if memory_index == Some(index) => "memory",
        "user" => "user_input",
        _ => "other",
    }
}

/// Account for one request.
///
/// `memory_index` is the index of the message the memory preamble was
/// prepended to, when the turn had one.
#[must_use]
pub fn account(
    turn_id: &str,
    agent_alias: Option<&str>,
    messages: &[ChatMessage],
    memory_index: Option<usize>,
) -> ContextLedger {
    let mut buckets: Vec<ContextBucket> = Vec::new();
    let mut total_chars = 0usize;
    for (index, message) in messages.iter().enumerate() {
        let source = bucket_for(&message.role, &message.content, memory_index, index);
        total_chars += message.content.chars().count();
        match buckets.iter_mut().find(|bucket| bucket.source == source) {
            Some(bucket) => {
                bucket.messages += 1;
                bucket.chars += message.content.chars().count();
            }
            None => buckets.push(ContextBucket {
                source: source.to_string(),
                messages: 1,
                chars: message.content.chars().count(),
            }),
        }
    }
    buckets.sort_by(|a, b| b.chars.cmp(&a.chars).then_with(|| a.source.cmp(&b.source)));
    ContextLedger {
        turn_id: turn_id.to_string(),
        agent_alias: agent_alias.map(str::to_string),
        buckets,
        total_messages: messages.len(),
        total_chars,
    }
}

/// The process-wide ledger.
fn store() -> &'static Mutex<Vec<ContextLedger>> {
    static STORE: OnceLock<Mutex<Vec<ContextLedger>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record a turn's context accounting.
pub fn record(ledger: ContextLedger) {
    if let Ok(mut ledgers) = store().lock() {
        ledgers.push(ledger);
        let capacity = LEDGER_CAPACITY;
        if ledgers.len() > capacity {
            let excess = ledgers.len() - capacity;
            ledgers.drain(0..excess);
        }
    }
}

/// The recorded ledgers, newest first.
#[must_use]
pub fn ledgers() -> Vec<ContextLedger> {
    let mut found = store().lock().map(|ledgers| ledgers.clone()).unwrap_or_default();
    found.reverse();
    found
}

/// The most recent ledger.
#[must_use]
pub fn latest() -> Option<ContextLedger> {
    store().lock().ok().and_then(|ledgers| ledgers.last().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn each_source_is_accounted_separately() {
        let messages = vec![
            message("system", "system prompt"),
            message("user", "the question"),
            message("tool", "the tool output"),
        ];
        let ledger = account("t1", Some("main"), &messages, None);
        assert_eq!(ledger.total_messages, 3);
        assert_eq!(
            ledger.total_chars,
            messages
                .iter()
                .map(|message| message.content.chars().count())
                .sum::<usize>()
        );
        assert_eq!(ledger.buckets.len(), 3);
        assert_eq!(ledger.buckets[0].source, "tool_output");
    }

    #[test]
    fn the_memory_preamble_is_its_own_bucket() {
        let messages = vec![message("user", "preamble + question")];
        let ledger = account("t1", None, &messages, Some(0));
        assert_eq!(ledger.buckets[0].source, "memory");
        assert_eq!(ledger.buckets[0].messages, 1);
    }

    #[test]
    fn externally_framed_content_is_its_own_bucket() {
        let messages = vec![message(
            "user",
            "<<<EXTERNAL_UNTRUSTED_CONTENT id=\"x\">>>\nhi\n<<<END_EXTERNAL_UNTRUSTED_CONTENT id=\"x\">>>",
        )];
        let ledger = account("t1", None, &messages, None);
        assert_eq!(ledger.buckets[0].source, "external_content");
    }

    #[test]
    fn a_share_is_the_fraction_of_the_total() {
        let messages = vec![message("system", "aaaa"), message("user", "bb")];
        let ledger = account("t1", None, &messages, None);
        assert!((ledger.share_of("system_prompt") - 66.6).abs() < 0.1);
        assert!((ledger.share_of("user_input") - 33.3).abs() < 0.1);
    }

    #[test]
    fn the_ledger_keeps_only_the_latest_turns() {
        for index in 0..(LEDGER_CAPACITY + 4) {
            record(account(&format!("t{index}"), None, &[], None));
        }
        let found = ledgers();
        assert_eq!(found.len(), LEDGER_CAPACITY);
        assert_eq!(found[0].turn_id, format!("t{}", LEDGER_CAPACITY + 3));
    }
}
