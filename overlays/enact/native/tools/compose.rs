//! `compose` — one call that assembles the context for a task.
//!
//! The chain this replaces is `content_search` → `file_read` → `file_read`:
//! three round trips, three chances to miss a file, and a lot of tokens spent
//! reading whole files to find the part that mattered. `compose` does the
//! ranking and the reading in one step and hands back the relevant source
//! with the file boundaries intact.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

use super::workspace_scan::{
    collect_workspace_files, display_path, estimate_tokens, is_source_path, keywords,
};

/// The default character budget for the assembled context.
pub const DEFAULT_MAX_CHARS: usize = 24_000;

/// The default number of files in the assembled context.
pub const DEFAULT_MAX_FILES: usize = 8;

/// The ceiling on how many workspace files the ranking looks at.
const MAX_CANDIDATES: usize = 400;

/// The largest a single file may be and still be inlined.
const MAX_FILE_CHARS: usize = 40_000;

/// Assemble the context for a task in one call.
pub struct ComposeTool {
    security: Arc<SecurityPolicy>,
}

impl ComposeTool {
    /// Build the tool with the agent's security policy.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }

    /// The candidate files, either the ones the caller named or the workspace.
    fn candidates(&self, args: &serde_json::Value) -> Vec<std::path::PathBuf> {
        let named = args
            .get("paths")
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(|path| self.security.resolve_tool_path(path))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !named.is_empty() {
            return named;
        }
        collect_workspace_files(
            &self.security.workspace_dir,
            &self.security,
            MAX_CANDIDATES,
        )
    }
}

/// One scored candidate.
struct Scored {
    path: std::path::PathBuf,
    score: usize,
}

/// What a keyword in a file's path is worth.
///
/// The path is the name the author gave the thing; the body is everything
/// that happens to live next to it. The path therefore wins outright: this
/// is more than the most the body can contribute for one keyword, so a name
/// that matches is never outranked by a file that merely repeats the word.
const PATH_MATCH_WEIGHT: usize = 9;

/// The most the body can contribute for one keyword.
///
/// Repetition is a weak signal, and an unbounded count would let one noisy
/// file dominate the ranking.
const BODY_MATCH_CAP: usize = 8;

/// Score a file against the task's keywords.
///
/// This is the relevance score only: zero means the file never mentions the
/// task. The source-file bonus is applied separately, at ranking time, so
/// that a file which matched nothing cannot be mistaken for a weak match.
fn score(path: &std::path::Path, content: &str, task_terms: &[String]) -> usize {
    let haystack_path = path.to_string_lossy().to_lowercase();
    let haystack_body = content.to_lowercase();
    let mut score = 0;
    for term in task_terms {
        if haystack_path.contains(term.as_str()) {
            score += PATH_MATCH_WEIGHT;
        }
        let occurrences = haystack_body.matches(term.as_str()).count();
        score += occurrences.min(BODY_MATCH_CAP);
    }
    score
}

/// The bonus a source file earns for being source.
///
/// Among the files that matched, the code comes before the documentation.
fn source_bonus(path: &std::path::Path) -> usize {
    if is_source_path(path) { 1 } else { 0 }
}

/// Rank the candidates and inline the best ones within the budget.
fn assemble(
    workspace: &std::path::Path,
    candidates: &[std::path::PathBuf],
    task: &str,
    max_chars: usize,
    max_files: usize,
) -> String {
    let task_terms = keywords(task);
    let mut scored: Vec<Scored> = Vec::new();
    let mut matched_any = false;
    for path in candidates {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let content = if content.chars().count() > MAX_FILE_CHARS {
            content.chars().take(MAX_FILE_CHARS).collect()
        } else {
            content
        };
        let relevance = score(path, &content, &task_terms);
        if relevance > 0 {
            matched_any = true;
        }
        scored.push(Scored {
            path: path.clone(),
            score: relevance + source_bonus(path),
        });
    }
    // Nothing matched the task: say so rather than handing back the workspace
    // and letting the model read files that have nothing to do with it.
    if !matched_any && !task_terms.is_empty() {
        return format!(
            "No file matched the task \"{task}\". Try naming paths explicitly, or widen the task wording.\n"
        );
    }
    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));

    let mut out = String::new();
    let mut used = 0usize;
    let mut included = 0usize;
    for entry in scored {
        if included >= max_files {
            break;
        }
        let Ok(content) = std::fs::read_to_string(&entry.path) else {
            continue;
        };
        let body: String = content.chars().take(MAX_FILE_CHARS).collect();
        let header = format!("── {} ──\n", display_path(workspace, &entry.path));
        let cost = header.len() + body.len();
        if used + cost > max_chars && included > 0 {
            break;
        }
        used += cost;
        included += 1;
        out.push_str(&header);
        out.push_str(&body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    if included == 0 {
        return format!(
            "No file matched the task \"{task}\". Try naming paths explicitly, or widen the task wording.\n"
        );
    }
    format!(
        "{included} file(s), {used} characters (~{} tokens) of context for \"{task}\".\n\n{out}",
        estimate_tokens(&out)
    )
}

#[async_trait]
impl Tool for ComposeTool {
    fn name(&self) -> &str {
        "compose"
    }

    fn description(&self) -> &str {
        "Assemble the context for a task in one call: rank the workspace files by relevance to the task and inline the best ones with their file boundaries intact. Replaces a search-then-read-then-read chain. Give it the task in your own words; optionally name candidate paths with `paths`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "What you are about to do, in your own words. The workspace files are ranked against this."
                },
                "paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Candidate files to rank instead of scanning the whole workspace. Relative paths resolve from the workspace root."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Character budget for the assembled context (default 24000)."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files to inline (default 8)."
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task = args
            .get("task")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow::Error::msg("Missing 'task' parameter"))?;
        let max_chars = args
            .get("max_chars")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_CHARS)
            .clamp(1_000, 400_000);
        let max_files = args
            .get("max_files")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_FILES)
            .clamp(1, 64);

        let candidates = self.candidates(&args);
        let assembled = assemble(
            &self.security.workspace_dir,
            &candidates,
            task,
            max_chars,
            max_files,
        );
        Ok(ToolResult {
            success: true,
            output: ToolOutput::text(assembled),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write(root: &std::path::Path, relative: &str, content: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn a_path_keyword_outranks_a_body_keyword() {
        let terms = keywords("parse invoice");
        let path_score = score(Path::new("src/parse.rs"), "nothing here", &terms);
        let body_score = score(Path::new("src/other.rs"), "parse parse parse", &terms);
        assert!(path_score > body_score);
    }

    #[test]
    fn a_file_that_never_mentions_the_task_scores_zero() {
        let terms = keywords("invoice");
        assert_eq!(score(Path::new("src/a.rs"), "unrelated", &terms), 0);
    }

    #[test]
    fn assembly_ranks_the_matching_file_first() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "src/invoice.rs", "fn invoice() {}\n");
        write(root.path(), "src/other.rs", "fn unrelated() {}\n");
        let candidates = vec![
            root.path().join("src/invoice.rs"),
            root.path().join("src/other.rs"),
        ];
        let out = assemble(root.path(), &candidates, "invoice", 10_000, 8);
        let invoice_at = out.find("invoice.rs").expect("invoice.rs must be included");
        let other_at = out.find("other.rs").expect("other.rs must be included");
        assert!(invoice_at < other_at, "the matching file must come first");
    }

    #[test]
    fn assembly_respects_the_file_budget() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "a.rs", "fn invoice() {}\n");
        write(root.path(), "b.rs", "fn invoice() {}\n");
        write(root.path(), "c.rs", "fn invoice() {}\n");
        let candidates: Vec<_> = ["a.rs", "b.rs", "c.rs"]
            .iter()
            .map(|name| root.path().join(name))
            .collect();
        let out = assemble(root.path(), &candidates, "invoice", 10_000, 2);
        assert!(out.contains("2 file(s)"));
    }

    #[test]
    fn a_task_that_matches_nothing_says_so() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "a.rs", "fn unrelated() {}\n");
        let candidates = vec![root.path().join("a.rs")];
        let out = assemble(root.path(), &candidates, "invoice", 10_000, 8);
        assert!(out.contains("No file matched"));
    }
}
