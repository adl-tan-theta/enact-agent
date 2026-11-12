//! Split an LLM request into trusted and untrusted context.
//!
//! The `before_llm_call` path hands a hook the whole message vector. Before
//! this split existed, a hook that wanted to sanitize, rewrite, or audit the
//! request could not tell the operator's own instructions from text that
//! arrived from somewhere else — so a handler that rewrote "the untrusted
//! part" had to rewrite the system prompt too.
//!
//! The rules here are deliberately conservative: content that *looks*
//! externally-authored is untrusted, and anything the runtime cannot prove
//! external is trusted only when the turn itself is trusted.

use zeroclaw_api::hook::{LlmCallHookContext, MessageTrust};
use zeroclaw_api::ingress::TrustClass;
use zeroclaw_api::model_provider::ChatMessage;

/// The marker the runtime uses to frame externally-authored content.
///
/// `frame_untrusted` in the security module writes this around a payload it
/// has already classified as external, so its presence is proof the content
/// came from outside.
const EXTERNAL_UNTRUSTED_MARKER: &str = "<<<EXTERNAL_UNTRUSTED_CONTENT";

/// Classify every message of an LLM request.
///
/// `turn_trust` is the trust class of the turn that produced the request, or
/// `None` when the caller has no turn envelope to consult (the max-iteration
/// path, for example). With `None`, only the rules that do not depend on the
/// turn apply.
///
/// The rules, applied in order:
///
/// 1. A message whose content carries the external-untrusted frame is
///    untrusted whatever its role: the frame is the runtime's own marker for
///    externally-authored content.
/// 2. A `tool` message is untrusted: tool output is data, never instruction.
/// 3. A `user` message is untrusted when the turn's trust class is
///    `Untrusted`: it arrived from outside.
/// 4. Everything else is trusted: the system prompt is the operator's own
///    configuration, and the agent's own prior turns are its own output.
#[must_use]
pub fn classify_messages(
    messages: &[ChatMessage],
    turn_trust: Option<TrustClass>,
) -> LlmCallHookContext {
    let message_trust = messages
        .iter()
        .map(|message| classify_one(message, turn_trust))
        .collect();
    LlmCallHookContext {
        turn_trust: turn_trust.unwrap_or(TrustClass::Trusted),
        message_trust,
    }
}

/// The trust of one message.
#[must_use]
pub fn classify_one(message: &ChatMessage, turn_trust: Option<TrustClass>) -> MessageTrust {
    if message.content.contains(EXTERNAL_UNTRUSTED_MARKER) {
        return MessageTrust::Untrusted;
    }
    match message.role.as_str() {
        "tool" => MessageTrust::Untrusted,
        "user" if turn_trust == Some(TrustClass::Untrusted) => MessageTrust::Untrusted,
        _ => MessageTrust::Trusted,
    }
}

/// The untrusted messages of a request, in order.
#[must_use]
pub fn untrusted_messages(messages: &[ChatMessage], context: &LlmCallHookContext) -> Vec<String> {
    messages
        .iter()
        .enumerate()
        .filter(|(index, _)| context.is_untrusted(*index))
        .map(|(_, message)| message.content.clone())
        .collect()
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
    fn a_system_prompt_is_trusted() {
        let messages = vec![message("system", "You are Enact.")];
        let context = classify_messages(&messages, Some(TrustClass::Trusted));
        assert!(!context.is_untrusted(0));
        assert_eq!(context.untrusted_count(), 0);
        assert_eq!(context.trusted_count(messages.len()), 1);
    }

    #[test]
    fn tool_output_is_untrusted_whatever_the_turn_is() {
        let messages = vec![message("tool", "the file says hello")];
        let context = classify_messages(&messages, Some(TrustClass::Trusted));
        assert!(context.is_untrusted(0));
        assert_eq!(context.trusted_count(messages.len()), 0);
    }

    #[test]
    fn a_user_message_follows_the_turn_trust() {
        let messages = vec![message("user", "do the thing")];
        let trusted = classify_messages(&messages, Some(TrustClass::Trusted));
        assert!(!trusted.is_untrusted(0));
        let untrusted = classify_messages(&messages, Some(TrustClass::Untrusted));
        assert!(untrusted.is_untrusted(0));
    }

    #[test]
    fn a_user_message_stays_trusted_without_a_turn_envelope() {
        let messages = vec![message("user", "do the thing")];
        let context = classify_messages(&messages, None);
        assert!(!context.is_untrusted(0));
    }

    #[test]
    fn a_framed_external_payload_is_untrusted_in_any_role() {
        let framed = "<<<EXTERNAL_UNTRUSTED_CONTENT id=\"x\">>>\nhi\n<<<END_EXTERNAL_UNTRUSTED_CONTENT id=\"x\">>>";
        let messages = vec![message("user", framed), message("system", "instructions")];
        let context = classify_messages(&messages, Some(TrustClass::Trusted));
        assert!(context.is_untrusted(0));
        assert!(!context.is_untrusted(1));
    }

    #[test]
    fn the_split_reports_both_halves() {
        let messages = vec![
            message("system", "instructions"),
            message("user", "hello"),
            message("tool", "output"),
        ];
        let context = classify_messages(&messages, Some(TrustClass::Untrusted));
        assert_eq!(context.untrusted_count(), 2);
        assert_eq!(context.trusted_count(messages.len()), 1);
        assert_eq!(context.untrusted_indices(), vec![1, 2]);
    }

    #[test]
    fn the_untrusted_half_can_be_collected() {
        let messages = vec![
            message("system", "instructions"),
            message("user", "hello"),
            message("tool", "output"),
        ];
        let context = classify_messages(&messages, Some(TrustClass::Untrusted));
        assert_eq!(
            untrusted_messages(&messages, &context),
            vec!["hello".to_string(), "output".to_string()]
        );
    }

    #[test]
    fn an_empty_request_classifies_to_nothing() {
        let context = classify_messages(&[], Some(TrustClass::Untrusted));
        assert_eq!(context.untrusted_count(), 0);
        assert_eq!(context.trusted_count(0), 0);
    }
}
