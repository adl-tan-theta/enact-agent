//! Read modes for `file_read`.
//!
//! Reading a whole file to find one function is the most common way an agent
//! burns context. The modes here answer the smaller questions directly:
//! what is in this file (`signatures`, `outline`), what changed (`diff`),
//! what is around this match (`anchored`), and what of this file is about my
//! task (`task`).
//!
//! Every mode returns text with line numbers, so a follow-up read can point
//! at exactly the lines that matter.

use std::path::Path;

use zeroclaw_api::tool::{ToolOutput, ToolResult};

use super::workspace_scan::keywords;

/// The read modes `file_read` understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// The signatures the file declares.
    Signatures,
    /// The structural outline of the file.
    Outline,
    /// The difference between the file and a base.
    Diff,
    /// A window around a match.
    Anchored,
    /// The parts of the file relevant to a task.
    Task,
}

impl ReadMode {
    /// The wire name of the mode.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ReadMode::Signatures => "signatures",
            ReadMode::Outline => "outline",
            ReadMode::Diff => "diff",
            ReadMode::Anchored => "anchored",
            ReadMode::Task => "task",
        }
    }
}

/// Parse the `mode` argument.
///
/// `Ok(None)` means "read the whole file", which is what an absent mode and
/// the explicit `full` mode both mean.
pub fn parse_mode(value: Option<&str>) -> Result<Option<ReadMode>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        "full" => Ok(None),
        "signatures" => Ok(Some(ReadMode::Signatures)),
        "outline" => Ok(Some(ReadMode::Outline)),
        "diff" => Ok(Some(ReadMode::Diff)),
        "anchored" => Ok(Some(ReadMode::Anchored)),
        "task" => Ok(Some(ReadMode::Task)),
        other => Err(format!(
            "unknown read mode '{other}' (expected one of: full, signatures, outline, diff, anchored, task)"
        )),
    }
}

/// The keywords that introduce a declaration.
const DECLARATION_KEYWORDS: &[&str] = &[
    "fn ", "func ", "def ", "function ", "struct ", "enum ", "trait ", "impl ", "mod ", "class ",
    "interface ", "type ", "const ", "static ", "let ", "var ", "pub ", "async ", "unsafe ",
];

/// The keywords that introduce a binding rather than a declaration.
///
/// A binding is worth showing in an outline — it says what the block holds —
/// but it is not a signature, so it stays out of the signature list.
const BINDING_KEYWORDS: &[&str] = &["let ", "var "];

/// Whether a line declares something.
fn is_declaration(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
        return false;
    }
    DECLARATION_KEYWORDS
        .iter()
        .any(|keyword| trimmed.starts_with(keyword))
}

/// Whether a line binds a name rather than declaring one.
fn is_binding(line: &str) -> bool {
    let trimmed = line.trim_start();
    BINDING_KEYWORDS
        .iter()
        .any(|keyword| trimmed.starts_with(keyword))
}

/// The signatures a file declares.
#[must_use]
pub fn render_signatures(content: &str) -> String {
    let mut out = String::new();
    for (index, line) in content.lines().enumerate() {
        if !is_declaration(line) || is_binding(line) {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let shortened: String = trimmed.chars().take(140).collect();
        out.push_str(&format!("{}: {shortened}\n", index + 1));
    }
    if out.is_empty() {
        out.push_str("No declarations found.\n");
    }
    out
}

/// The structural outline of a file.
///
/// The nesting comes from the brace depth for brace languages and from the
/// indentation for the rest, which covers most of what an agent needs to
/// orient itself.
#[must_use]
pub fn render_outline(content: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut pending_indent: Option<usize> = None;
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let opens = trimmed.chars().filter(|character| *character == '{').count();
        let closes = trimmed.chars().filter(|character| *character == '}').count();
        if !is_declaration(line) {
            depth = depth.saturating_add(opens);
            depth = depth.saturating_sub(closes);
            continue;
        }
        let indent = match pending_indent {
            Some(indent) => {
                pending_indent = None;
                indent
            }
            None => {
                let spaces = line.len() - line.trim_start().len();
                if trimmed.contains('{') {
                    depth
                } else {
                    spaces / 4
                }
            }
        };
        let shortened: String = trimmed.chars().take(140).collect();
        out.push_str(&format!(
            "{:indent$}{}: {shortened}\n",
            "",
            index + 1,
            indent = indent * 2
        ));
        // A declaration that opens a block raises the depth for what follows
        // it: the members of an `impl` or a `struct` nest under it.
        depth = depth.saturating_add(opens);
        depth = depth.saturating_sub(closes);
        if !trimmed.contains('{') && trimmed.ends_with(';') {
            pending_indent = Some(indent);
        }
    }
    if out.is_empty() {
        out.push_str("No declarations found.\n");
    }
    out
}

/// A line-by-line difference between two texts.
///
/// The report names the lines that changed and the lines around them, which
/// is what a review needs; a full unified diff is not worth the tokens.
#[must_use]
pub fn render_diff(before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut out = String::new();
    let mut changed = 0usize;
    let mut index = 0usize;
    while index < before_lines.len() || index < after_lines.len() {
        let old = before_lines.get(index);
        let new = after_lines.get(index);
        if old == new {
            index += 1;
            continue;
        }
        changed += 1;
        out.push_str(&format!("@@ line {}\n", index + 1));
        if let Some(old) = old {
            out.push_str(&format!("-{old}\n"));
        }
        if let Some(new) = new {
            out.push_str(&format!("+{new}\n"));
        }
        index += 1;
    }
    if changed == 0 {
        out.push_str("No differences.\n");
    } else {
        out.push_str(&format!("{changed} changed line(s).\n"));
    }
    out
}

/// The lines around a match.
///
/// The anchor is matched case-insensitively as a substring. Every match is
/// shown with `context` lines around it, and the regions are merged so an
/// overlapping match is not printed twice.
#[must_use]
pub fn render_anchored(content: &str, anchor: &str, context: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let needle = anchor.to_lowercase();
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains(&needle) {
            let start = index.saturating_sub(context);
            let end = (index + context + 1).min(lines.len());
            match regions.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => regions.push((start, end)),
            }
        }
    }
    if regions.is_empty() {
        return format!("No line matches '{anchor}'.\n");
    }
    let mut out = String::new();
    let mut previous_end = 0usize;
    for (start, end) in regions {
        if start > previous_end {
            out.push_str("...\n");
        }
        for index in start..end {
            out.push_str(&format!("{}: {}\n", index + 1, lines[index]));
        }
        previous_end = end;
    }
    out
}

/// The parts of a file that are about a task.
///
/// Lines are scored by how many of the task's words they contain, and the
/// best-scoring regions are returned in file order.
#[must_use]
pub fn render_task(content: &str, task: &str, context: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let terms = keywords(task);
    if terms.is_empty() {
        return "The task has no searchable words.\n".to_string();
    }
    let mut scored: Vec<(usize, usize)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let lowered = line.to_lowercase();
        let hits = terms
            .iter()
            .filter(|term| lowered.contains(term.as_str()))
            .count();
        if hits > 0 {
            scored.push((index, hits));
        }
    }
    if scored.is_empty() {
        return format!("No line mentions anything from the task \"{task}\".\n");
    }
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for (index, _) in scored {
        let start = index.saturating_sub(context);
        let end = (index + context + 1).min(lines.len());
        match regions.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => regions.push((start, end)),
        }
    }
    let mut out = String::new();
    let mut previous_end = 0usize;
    for (start, end) in regions {
        if start > previous_end {
            out.push_str("...\n");
        }
        for index in start..end {
            out.push_str(&format!("{}: {}\n", index + 1, lines[index]));
        }
        previous_end = end;
    }
    out
}

/// Render a mode against a file's contents.
#[must_use]
pub fn render(
    mode: ReadMode,
    contents: &str,
    path: &Path,
    args: &serde_json::Value,
) -> ToolResult {
    let text = match mode {
        ReadMode::Signatures => render_signatures(contents),
        ReadMode::Outline => render_outline(contents),
        ReadMode::Diff => {
            let Some(base) = args.get("base").and_then(|value| value.as_str()) else {
                return ToolResult {
                    success: false,
                    output: ToolOutput::text(
                        "The diff mode compares the file against a base. Pass `base` with the previous text, or `base_path` with the previous file.\n",
                    ),
                    error: Some("the diff mode needs a base".to_string()),
                };
            };
            render_diff(base, contents)
        }
        ReadMode::Anchored => {
            let Some(anchor) = args.get("anchor").and_then(|value| value.as_str()) else {
                return ToolResult {
                    success: false,
                    output: ToolOutput::text(
                        "The anchored mode needs an `anchor`: the text to find.\n",
                    ),
                    error: Some("the anchored mode needs an anchor".to_string()),
                };
            };
            let context = args
                .get("context")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(3)
                .clamp(0, 50);
            render_anchored(contents, anchor, context)
        }
        ReadMode::Task => {
            let Some(task) = args.get("task").and_then(|value| value.as_str()) else {
                return ToolResult {
                    success: false,
                    output: ToolOutput::text("The task mode needs a `task` to look for.\n"),
                    error: Some("the task mode needs a task".to_string()),
                };
            };
            let context = args
                .get("context")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(3)
                .clamp(0, 50);
            render_task(contents, task, context)
        }
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    ToolResult {
        success: true,
        output: ToolOutput::text(format!("{name} — {}\n{text}", mode.as_str())),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
use std::fs;

pub fn parse_config(path: &str) -> Config {
    let text = read(path);
    Config::new(text)
}

struct Config {
    name: String,
}

impl Config {
    fn new(text: String) -> Self {
        Self { name: text }
    }
}
";

    #[test]
    fn an_absent_mode_reads_the_whole_file() {
        assert_eq!(parse_mode(None), Ok(None));
        assert_eq!(parse_mode(Some("full")), Ok(None));
    }

    #[test]
    fn an_unknown_mode_is_rejected() {
        assert!(parse_mode(Some("nonsense")).is_err());
    }

    #[test]
    fn every_mode_has_a_wire_name() {
        assert_eq!(parse_mode(Some("signatures")), Ok(Some(ReadMode::Signatures)));
        assert_eq!(ReadMode::Outline.as_str(), "outline");
        assert_eq!(ReadMode::Diff.as_str(), "diff");
        assert_eq!(ReadMode::Anchored.as_str(), "anchored");
        assert_eq!(ReadMode::Task.as_str(), "task");
    }

    #[test]
    fn signatures_list_the_declarations_with_their_lines() {
        let out = render_signatures(SAMPLE);
        assert!(out.contains("3: pub fn parse_config"), "{out}");
        assert!(out.contains("8: struct Config"), "{out}");
        assert!(out.contains("12: impl Config"), "{out}");
        assert!(!out.contains("use std::fs"), "{out}");
        // A binding is not a signature.
        assert!(!out.contains("let text"), "{out}");
    }

    #[test]
    fn the_outline_nests_by_depth() {
        let out = render_outline(SAMPLE);
        assert!(out.contains("3: pub fn parse_config"), "{out}");
        assert!(out.contains("8: struct Config"), "{out}");
        assert!(out.contains("12: impl Config"), "{out}");
        // The members of an `impl` nest under it.
        assert!(out.contains("  13: fn new"), "{out}");
    }

    #[test]
    fn a_diff_reports_the_changed_lines() {
        let out = render_diff("a\nb\nc\n", "a\nB\nc\n");
        assert!(out.contains("-b"));
        assert!(out.contains("+B"));
        assert!(out.contains("changed line"));
    }

    #[test]
    fn an_unchanged_diff_says_so() {
        assert!(render_diff("a\n", "a\n").contains("No differences"));
    }

    #[test]
    fn an_anchor_shows_the_matching_line_and_its_neighbours() {
        let out = render_anchored(SAMPLE, "read(path)", 1);
        assert!(out.contains("3:"));
        assert!(out.contains("read(path)"));
    }

    #[test]
    fn an_anchor_that_matches_nothing_says_so() {
        assert!(render_anchored(SAMPLE, "zzz-not-here", 2).contains("No line matches"));
    }

    #[test]
    fn a_task_returns_the_lines_that_mention_it() {
        let out = render_task(SAMPLE, "config name", 1);
        assert!(out.contains("parse_config"));
        assert!(out.contains("name"));
    }

    #[test]
    fn a_task_that_matches_nothing_says_so() {
        assert!(render_task(SAMPLE, "zzz-not-here", 1).contains("No line mentions"));
    }
}
