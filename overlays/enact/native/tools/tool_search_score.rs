//! Relevance scoring for `tool_search` keyword queries.
//!
//! The first matcher counted how many query terms appeared verbatim in a
//! tool's name and description. That is a literal substring count: it cannot
//! tell a name match from a passing mention in a sentence, it gives every
//! mention the same weight, and it has no ceiling on how much a result may
//! cost.
//!
//! This module scores a query the way the canonical registry's own search
//! does, so the same query ranks the same tools here:
//!
//! - a name that *is* the query outranks one that starts with it, which
//!   outranks one that merely contains it;
//! - every query word landing in the name is a strong signal, and a name
//!   that holds *every* word is a stronger signal still;
//! - an action name, an alias and a description mention are weaker signals,
//!   each worth less than a name hit.
//!
//! The result set is bounded twice: by a reference count and by a token
//! budget, so a broad query cannot return a prompt-sized payload.

/// At most this many references a single search may return.
pub const MAX_SEARCH_TOOL_REFERENCES: usize = 32;

/// The token budget a single search's result may cost.
pub const SEARCH_TOKEN_BUDGET: usize = 6000;

/// Characters per token, for the budget estimate.
pub const CHARS_PER_TOKEN: usize = 4;

/// Words that carry no signal about which tool is meant.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "in", "on", "for", "with", "and", "or", "is", "it", "its", "my", "me",
    "do", "can", "how", "what", "when", "where", "which", "that", "this", "by", "be", "as", "at",
    "from", "any", "some",
];

/// The score a name that is exactly the query earns.
const SCORE_NAME_EXACT: usize = 100;
/// The score a name that starts with the query earns.
const SCORE_NAME_PREFIX: usize = 60;
/// The score a name that contains the query earns.
const SCORE_NAME_CONTAINS: usize = 40;
/// The score each query word earns when it lands in the name.
const SCORE_NAME_WORD: usize = 20;
/// The score each query word earns when it lands in an action name.
const SCORE_ACTION_WORD: usize = 6;
/// The score each query word earns when it lands in the description.
const SCORE_DESCRIPTION_WORD: usize = 5;
/// The score each query word earns when it lands in an alias.
const SCORE_ALIAS_WORD: usize = 4;
/// The bonus a name earns when it holds *every* query word.
const SCORE_NAME_ALL_WORDS: usize = 25;

/// The query words, with the stop words dropped.
///
/// A one-letter token is dropped too: it is a prefix of many names and says
/// nothing about which one is meant.
#[must_use]
pub fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() > 1 && !STOP_WORDS.contains(&token.as_str()))
        .collect()
}

/// One candidate a query is scored against.
pub struct SearchCandidate<'a> {
    /// The tool's canonical name.
    pub name: &'a str,
    /// The tool's description.
    pub description: &'a str,
    /// The action names the tool exposes, when it is a consolidated family.
    pub actions: &'a [String],
    /// The legacy names the tool replaced, when it is a consolidated family.
    pub aliases: &'a [String],
}

/// The score a candidate earns, and why.
///
/// The score is what orders the results; the matched-on list says which
/// signals fired, so the ranking can be explained rather than guessed at.
#[must_use]
pub fn score_candidate(candidate: &SearchCandidate<'_>, query: &str, words: &[String]) -> (usize, Vec<String>) {
    let mut score = 0usize;
    let mut matched = Vec::new();
    let name = candidate.name.to_ascii_lowercase();
    let query_lower = query.to_ascii_lowercase();
    let description = candidate.description.to_ascii_lowercase();

    if name == query_lower {
        score += SCORE_NAME_EXACT;
        matched.push("name==".to_string());
    } else if name.starts_with(&query_lower) {
        score += SCORE_NAME_PREFIX;
        matched.push("name^".to_string());
    } else if name.contains(&query_lower) {
        score += SCORE_NAME_CONTAINS;
        matched.push("name~".to_string());
    }

    let mut name_hits = 0usize;
    for word in words {
        if word.is_empty() {
            continue;
        }
        if name.contains(word.as_str()) {
            score += SCORE_NAME_WORD;
            name_hits += 1;
            matched.push(format!("name:{word}"));
        }
        if description.contains(word.as_str()) {
            score += SCORE_DESCRIPTION_WORD;
            matched.push(format!("desc:{word}"));
        }
        if candidate
            .actions
            .iter()
            .any(|action| action.to_ascii_lowercase().contains(word.as_str()))
        {
            score += SCORE_ACTION_WORD;
            matched.push(format!("action:{word}"));
        }
        if candidate
            .aliases
            .iter()
            .any(|alias| alias.to_ascii_lowercase().contains(word.as_str()))
        {
            score += SCORE_ALIAS_WORD;
            matched.push(format!("alias:{word}"));
        }
    }

    // Every word landing in the name is a much stronger signal than a
    // scatter across name and description.
    if !words.is_empty() && name_hits == words.len() {
        score += SCORE_NAME_ALL_WORDS;
        matched.push("name:all".to_string());
    }

    (score, matched)
}

/// The token cost of one result.
///
/// The estimate is deliberately coarse: the budget exists to stop a broad
/// query from returning a prompt-sized payload, not to predict a tokenizer
/// exactly.
#[must_use]
pub fn result_token_cost(description: &str, action_count: usize) -> usize {
    description.len() / CHARS_PER_TOKEN + action_count * 2 + 12
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate<'a>(name: &'a str, description: &'a str) -> SearchCandidate<'a> {
        SearchCandidate {
            name,
            description,
            actions: &[],
            aliases: &[],
        }
    }

    #[test]
    fn stop_words_and_single_letters_are_dropped() {
        assert_eq!(
            query_tokens("Add a recurring job to the schedule"),
            vec!["add", "recurring", "job", "schedule"]
        );
        assert!(query_tokens("a to of the").is_empty());
    }

    #[test]
    fn an_exact_name_outranks_a_prefix_outranks_a_containment() {
        let words = query_tokens("cron");
        let exact = score_candidate(&candidate("cron", "x"), "cron", &words).0;
        let prefix = score_candidate(&candidate("cron_add", "x"), "cron", &words).0;
        let contains = score_candidate(&candidate("my_cron_thing", "x"), "cron", &words).0;
        assert!(exact > prefix, "{exact} > {prefix}");
        assert!(prefix > contains, "{prefix} > {contains}");
    }

    #[test]
    fn a_word_in_the_name_outranks_the_same_word_in_the_description() {
        let words = query_tokens("recurring");
        let in_name = score_candidate(&candidate("recurring_jobs", "x"), "recurring", &words).0;
        let in_description = score_candidate(&candidate("jobs", "a recurring job"), "recurring", &words).0;
        assert!(in_name > in_description, "{in_name} > {in_description}");
    }

    #[test]
    fn a_name_holding_every_word_earns_the_bonus() {
        let words = query_tokens("cron job");
        let both = score_candidate(&candidate("cron_job", "x"), "cron job", &words).0;
        let one = score_candidate(&candidate("cron", "a job"), "cron job", &words).0;
        assert!(both > one, "{both} > {one}");
    }

    #[test]
    fn an_action_name_is_a_signal() {
        let words = query_tokens("cron_add");
        let actions = vec!["cron_add".to_string()];
        let with_action = SearchCandidate {
            name: "family",
            description: "x",
            actions: &actions,
            aliases: &[],
        };
        let without = score_candidate(&candidate("family", "x"), "cron_add", &words).0;
        let with = score_candidate(&with_action, "cron_add", &words).0;
        assert!(with > without, "{with} > {without}");
    }

    #[test]
    fn an_alias_is_a_signal() {
        let words = query_tokens("schedule_add");
        let aliases = vec!["schedule_add".to_string()];
        let with_alias = SearchCandidate {
            name: "cron",
            description: "x",
            actions: &[],
            aliases: &aliases,
        };
        let without = score_candidate(&candidate("cron", "x"), "schedule_add", &words).0;
        let with = score_candidate(&with_alias, "schedule_add", &words).0;
        assert!(with > without, "{with} > {without}");
    }

    #[test]
    fn the_matched_on_list_names_the_signals_that_fired() {
        let words = query_tokens("cron");
        let (_, matched) = score_candidate(&candidate("cron_add", "a cron tool"), "cron", &words);
        assert!(matched.contains(&"name^".to_string()), "{matched:?}");
        assert!(matched.contains(&"name:cron".to_string()), "{matched:?}");
    }

    #[test]
    fn a_longer_description_costs_more_tokens() {
        let short = result_token_cost("short", 0);
        let long = result_token_cost(&"x".repeat(400), 0);
        assert!(long > short, "{long} > {short}");
        assert!(result_token_cost("x", 5) > result_token_cost("x", 0));
    }
}
