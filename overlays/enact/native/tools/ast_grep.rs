//! `ast_grep` and `ast_edit` — structural code queries and previewed rewrites.
//!
//! Both tools drive the `ast-grep` binary, which parses the code with
//! tree-sitter and matches on syntax rather than on text. A content search
//! cannot tell `foo(a, b)` from `foo(a);` or from the word `foo` in a
//! comment; an AST pattern can.
//!
//! `ast_grep` answers "where does this shape appear". `ast_edit` answers
//! "what would this rewrite change" — and only changes it when the caller
//! says so.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// The default number of matches a search reports.
pub const DEFAULT_MAX_RESULTS: usize = 50;

/// The binaries searched for, in order.
const BINARY_NAMES: &[&str] = &["ast-grep", "sg"];

/// The message shown when the binary is not installed.
const MISSING_BINARY: &str = "ast-grep is not installed, so structural search is unavailable. \
Install it with `npm i -g @ast-grep/cli` (or `cargo install ast-grep --locked`), \
or use content_search for a text-level search.";

/// One structural match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstMatch {
    /// The file the match is in.
    pub file: String,
    /// The one-based line the match starts on.
    pub line: usize,
    /// The one-based column the match starts at.
    pub column: usize,
    /// The matched text.
    pub text: String,
    /// The text the match would be replaced with, when a rewrite was asked for.
    pub replacement: Option<String>,
    /// The byte range the replacement covers.
    pub replacement_range: Option<(usize, usize)>,
}

/// The `ast-grep` binary, if it is installed.
#[must_use]
pub fn find_binary() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for name in BINARY_NAMES {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Run ast-grep and return its stdout.
async fn run_ast_grep(binary: &Path, args: &[String]) -> anyhow::Result<String> {
    let child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| anyhow::anyhow!("could not run ast-grep: {error}"))?;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("ast-grep timed out"))?
    .map_err(|error| anyhow::anyhow!("ast-grep failed: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(
            "ast-grep exited with {}{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
    }
    Ok(stdout)
}

/// Parse ast-grep's JSON-stream output into matches.
fn parse_matches(stdout: &str) -> Vec<AstMatch> {
    let mut matches = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(file) = value.get("file").and_then(Value::as_str) else {
            continue;
        };
        let start = value.get("range").and_then(|range| range.get("start"));
        let line_number = start
            .and_then(|start| start.get("line"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize
            + 1;
        let column = start
            .and_then(|start| start.get("column"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize
            + 1;
        let replacement_range = value
            .get("replacementOffsets")
            .and_then(|offsets| {
                Some((
                    offsets.get("start")?.as_u64()? as usize,
                    offsets.get("end")?.as_u64()? as usize,
                ))
            });
        matches.push(AstMatch {
            file: file.to_string(),
            line: line_number,
            column,
            text: value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            replacement: value
                .get("replacement")
                .and_then(Value::as_str)
                .map(str::to_string),
            replacement_range,
        });
    }
    matches
}

/// Render a set of matches as a readable report.
fn render_matches(matches: &[AstMatch], workspace: &Path) -> String {
    if matches.is_empty() {
        return "No structural match.\n".to_string();
    }
    let mut out = format!("{} match(es):\n", matches.len());
    for entry in matches {
        let path = Path::new(&entry.file);
        let file = path
            .strip_prefix(workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        out.push_str(&format!(
            "  {}:{}:{}  {}\n",
            file, entry.line, entry.column, entry.text
        ));
    }
    out
}

/// Apply a set of replacements to a file's text.
///
/// The replacements are applied from the end backwards so the earlier byte
/// offsets stay valid.
fn apply_replacements(text: &str, matches: &[AstMatch]) -> Option<String> {
    let mut edits: Vec<((usize, usize), &str)> = Vec::new();
    for entry in matches {
        let (start, end) = entry.replacement_range?;
        let replacement = entry.replacement.as_deref()?;
        edits.push(((start, end), replacement));
    }
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|((start, _), _)| std::cmp::Reverse(*start));
    let mut out = text.to_string();
    for ((start, end), replacement) in edits {
        if start > end || end > out.len() || !out.is_char_boundary(start) || !out.is_char_boundary(end)
        {
            return None;
        }
        out.replace_range(start..end, replacement);
    }
    Some(out)
}

/// A compact line diff between two texts.
fn line_diff(before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut out = String::new();
    let mut index = 0usize;
    while index < before_lines.len() || index < after_lines.len() {
        let old = before_lines.get(index);
        let new = after_lines.get(index);
        match (old, new) {
            (Some(old), Some(new)) if old == new => {
                index += 1;
            }
            (old, new) => {
                if let Some(old) = old {
                    out.push_str(&format!("-{old}\n"));
                }
                if let Some(new) = new {
                    out.push_str(&format!("+{new}\n"));
                }
                index += 1;
            }
        }
    }
    out
}

/// Search for a structural pattern.
pub struct AstGrepTool {
    security: Arc<SecurityPolicy>,
}

impl AstGrepTool {
    /// Build the tool with the agent's security policy.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

/// Rewrite a structural pattern, with a preview.
pub struct AstEditTool {
    security: Arc<SecurityPolicy>,
}

impl AstEditTool {
    /// Build the tool with the agent's security policy.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

/// The arguments every ast-grep run shares.
struct RunSpec<'a> {
    pattern: Option<&'a str>,
    kind: Option<&'a str>,
    replacement: Option<&'a str>,
    language: Option<&'a str>,
    paths: Vec<String>,
}

fn run_args(spec: &RunSpec<'_>) -> Vec<String> {
    let mut args = vec!["run".to_string()];
    if let Some(pattern) = spec.pattern {
        args.push("--pattern".to_string());
        args.push(pattern.to_string());
    }
    if let Some(kind) = spec.kind {
        args.push("--kind".to_string());
        args.push(kind.to_string());
    }
    if let Some(replacement) = spec.replacement {
        args.push("--rewrite".to_string());
        args.push(replacement.to_string());
    }
    if let Some(language) = spec.language {
        args.push("--lang".to_string());
        args.push(language.to_string());
    }
    args.push("--json=stream".to_string());
    args.extend(spec.paths.iter().cloned());
    args
}

#[async_trait]
impl Tool for AstGrepTool {
    fn name(&self) -> &str {
        "ast_grep"
    }

    fn description(&self) -> &str {
        "Search code by structure, not by text. Give an ast-grep pattern such as `foo($A)` or `fn $NAME($$$ARGS) { $$$BODY }` and it matches the syntax rather than the characters, so a renamed variable or a reformat does not hide it. Pass `kind` instead of `pattern` to select by syntax kind (for example `function_item`)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "An ast-grep pattern, e.g. `foo($A)` or `impl $T for $U { $$$BODY }`."
                },
                "kind": {
                    "type": "string",
                    "description": "An ast-grep syntax kind to select, e.g. `function_item`. Used instead of `pattern`."
                },
                "language": {
                    "type": "string",
                    "description": "The language of the pattern, e.g. `rust`, `typescript`, `python`. Inferred from the files when omitted."
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search. Defaults to the workspace root."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of matches to report (default 50)."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(binary) = find_binary() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::text(MISSING_BINARY),
                error: Some("ast-grep is not installed".to_string()),
            });
        };
        let pattern = args.get("pattern").and_then(Value::as_str);
        let kind = args.get("kind").and_then(Value::as_str);
        if pattern.is_none() && kind.is_none() {
            anyhow::bail!("Give either 'pattern' or 'kind'");
        }
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, 1_000);
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(|path| self.security.resolve_tool_path(path))
            .unwrap_or_else(|| self.security.workspace_dir.clone());
        let language = args.get("language").and_then(Value::as_str);
        let spec = RunSpec {
            pattern,
            kind,
            replacement: None,
            language,
            paths: vec![path.to_string_lossy().to_string()],
        };
        let stdout = run_ast_grep(&binary, &run_args(&spec)).await?;
        let mut matches = parse_matches(&stdout);
        matches.truncate(max_results);
        Ok(ToolResult {
            success: true,
            output: ToolOutput::text(render_matches(&matches, &self.security.workspace_dir)),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for AstEditTool {
    fn name(&self) -> &str {
        "ast_edit"
    }

    fn description(&self) -> &str {
        "Rewrite code by structure, with a preview. It reports what the rewrite would change and leaves the file alone unless you pass `dry_run: false`. Use it for changes that a text replace would get wrong: renaming a call without touching a comment, changing a function body without touching a similar string."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "An ast-grep pattern to match, e.g. `foo($A)`."
                },
                "replacement": {
                    "type": "string",
                    "description": "The text to replace the matched node with. May use the pattern's metavariables, e.g. `bar($A)`."
                },
                "path": {
                    "type": "string",
                    "description": "The file to rewrite. Relative paths resolve from the workspace root."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview only (default true). Pass false to write the change."
                },
                "language": {
                    "type": "string",
                    "description": "The language of the pattern, e.g. `rust`."
                }
            },
            "required": ["pattern", "replacement", "path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(binary) = find_binary() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::text(MISSING_BINARY),
                error: Some("ast-grep is not installed".to_string()),
            });
        };
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::Error::msg("Missing 'pattern' parameter"))?;
        let replacement = args
            .get("replacement")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::Error::msg("Missing 'replacement' parameter"))?;
        let path_arg = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::Error::msg("Missing 'path' parameter"))?;
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let language = args.get("language").and_then(Value::as_str);

        let path = self.security.resolve_tool_path(path_arg);
        let resolved = std::fs::canonicalize(&path)
            .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;
        if !self.security.is_resolved_path_readable(&resolved) {
            anyhow::bail!("Path not allowed by security policy: {path_arg}");
        }
        let before = std::fs::read_to_string(&resolved)
            .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;

        let spec = RunSpec {
            pattern: Some(pattern),
            kind: None,
            replacement: Some(replacement),
            language,
            paths: vec![resolved.to_string_lossy().to_string()],
        };
        let stdout = run_ast_grep(&binary, &run_args(&spec)).await?;
        let matches = parse_matches(&stdout);
        if matches.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: ToolOutput::text("No structural match; nothing to rewrite.\n"),
                error: None,
            });
        }
        let Some(after) = apply_replacements(&before, &matches) else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::text(
                    "The rewrite could not be applied cleanly to the file; nothing was changed.\n",
                ),
                error: Some("the rewrite did not apply cleanly".to_string()),
            });
        };
        if after == before {
            return Ok(ToolResult {
                success: true,
                output: ToolOutput::text("The rewrite matched, but changed nothing.\n"),
                error: None,
            });
        }

        let relative = resolved
            .strip_prefix(&self.security.workspace_dir)
            .unwrap_or(&resolved)
            .to_string_lossy()
            .to_string();
        let preview = format!(
            "{} match(es) in {relative}:\n\n{}",
            matches.len(),
            line_diff(&before, &after)
        );

        if dry_run {
            return Ok(ToolResult {
                success: true,
                output: ToolOutput::text(format!(
                    "{preview}\nPreview only. Re-run with dry_run=false to write this change.\n"
                )),
                error: None,
            });
        }

        if !self.security.is_resolved_path_allowed(&resolved) {
            anyhow::bail!("Path not writable by security policy: {path_arg}");
        }
        std::fs::write(&resolved, &after)
            .map_err(|error| anyhow::anyhow!("cannot write {}: {error}", resolved.display()))?;
        Ok(ToolResult {
            success: true,
            output: ToolOutput::text(format!("{preview}\nWritten to {relative}.\n")),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_parse_out_of_ast_greps_json_stream() {
        let stdout = r#"{"text":"compute(1)","range":{"byteOffset":{"start":24,"end":34},"start":{"line":1,"column":12},"end":{"line":1,"column":22}},"file":"/w/a.rs","lines":"    let x = compute(1);","charCount":{"leading":12,"trailing":1},"language":"Rust","metaVariables":{"single":{"A":{"text":"1","range":{"byteOffset":{"start":32,"end":33},"start":{"line":1,"column":20},"end":{"line":1,"column":21}}}},"multi":{},"transformed":{}}}"#;
        let matches = parse_matches(stdout);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, "/w/a.rs");
        assert_eq!(matches[0].line, 2);
        assert_eq!(matches[0].column, 13);
        assert_eq!(matches[0].text, "compute(1)");
        assert_eq!(matches[0].replacement, None);
    }

    #[test]
    fn a_rewrite_carries_its_replacement_and_range() {
        let stdout = r#"{"text":"compute(1)","range":{"byteOffset":{"start":24,"end":34},"start":{"line":1,"column":12},"end":{"line":1,"column":22}},"file":"/w/a.rs","replacement":"double(1)","replacementOffsets":{"start":24,"end":34}}"#;
        let matches = parse_matches(stdout);
        assert_eq!(matches[0].replacement.as_deref(), Some("double(1)"));
        assert_eq!(matches[0].replacement_range, Some((24, 34)));
    }

    #[test]
    fn replacements_apply_from_the_end_backwards() {
        let before = "aa bb aa";
        let matches = vec![
            AstMatch {
                file: "/w/a.rs".to_string(),
                line: 1,
                column: 1,
                text: "aa".to_string(),
                replacement: Some("xx".to_string()),
                replacement_range: Some((0, 2)),
            },
            AstMatch {
                file: "/w/a.rs".to_string(),
                line: 1,
                column: 7,
                text: "aa".to_string(),
                replacement: Some("yy".to_string()),
                replacement_range: Some((6, 8)),
            },
        ];
        assert_eq!(apply_replacements(before, &matches).as_deref(), Some("xx bb yy"));
    }

    #[test]
    fn a_replacement_without_a_range_is_not_applied() {
        let matches = vec![AstMatch {
            file: "/w/a.rs".to_string(),
            line: 1,
            column: 1,
            text: "aa".to_string(),
            replacement: Some("xx".to_string()),
            replacement_range: None,
        }];
        assert_eq!(apply_replacements("aa", &matches), None);
    }

    #[test]
    fn the_report_names_the_file_relative_to_the_workspace() {
        let matches = vec![AstMatch {
            file: "/w/src/a.rs".to_string(),
            line: 3,
            column: 5,
            text: "foo()".to_string(),
            replacement: None,
            replacement_range: None,
        }];
        let report = render_matches(&matches, Path::new("/w"));
        assert!(report.contains("src/a.rs:3:5"));
        assert!(report.contains("foo()"));
    }

    #[test]
    fn an_empty_result_says_so() {
        assert!(render_matches(&[], Path::new("/w")).contains("No structural match"));
    }

    #[test]
    fn the_run_arguments_carry_the_pattern_and_the_rewrite() {
        let spec = RunSpec {
            pattern: Some("foo($A)"),
            kind: None,
            replacement: Some("bar($A)"),
            language: Some("rust"),
            paths: vec!["/w".to_string()],
        };
        let args = run_args(&spec);
        assert!(args.contains(&"run".to_string()));
        assert!(args.contains(&"foo($A)".to_string()));
        assert!(args.contains(&"bar($A)".to_string()));
        assert!(args.contains(&"rust".to_string()));
        assert!(args.contains(&"--json=stream".to_string()));
    }
}
