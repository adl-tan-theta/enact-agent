//! `callgraph` and `graph` — blast radius, not just text search.
//!
//! A content search can tell you where a name is written. It cannot tell you
//! what a change would break, because the interesting edges are between
//! symbols, not between lines. `callgraph` answers "who calls this, and what
//! does this call"; `graph` answers the same question one level up, at
//! module and file granularity.
//!
//! The analysis is source-level and language-aware for Rust, and falls back
//! to a generic symbol scan for other languages. It is a heuristic, and it
//! says so: the answer is a map to start a review with, not a proof.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

use super::workspace_scan::{collect_workspace_files, display_path, is_source_path};

/// The default number of hops the graph walks.
pub const DEFAULT_DEPTH: usize = 2;

/// The ceiling on how many files one analysis reads.
const MAX_FILES: usize = 600;

/// One symbol found in the sources.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Symbol {
    /// The file the symbol is defined in.
    file: PathBuf,
    /// The one-based line of the definition.
    line: usize,
    /// The symbol's name.
    name: String,
}

/// A call edge: the function at the call site calls `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    /// The function the call site sits in, when the file declares one.
    from: String,
    /// The name being called.
    to: String,
    /// The file the call site is in.
    file: PathBuf,
    /// The one-based line of the call site.
    line: usize,
}

/// The symbol table and call edges of one workspace.
struct Analysis {
    symbols: BTreeMap<String, Symbol>,
    edges: Vec<Edge>,
}

/// The keywords that never introduce a definition.
const DEFINITION_KEYWORDS: &[&str] = &[
    "if", "for", "while", "match", "return", "else", "loop", "let", "fn", "impl", "struct", "enum",
    "class", "def", "function", "pub", "pub(crate)", "pub(super)", "async", "unsafe", "const",
    "static", "type", "trait", "where", "move", "ref", "mut", "in", "as", "use", "mod",
];

/// The name of a definition, if the line declares one.
fn definition_name(line: &str) -> Option<String> {
    // Modifiers stack: `pub(crate) async fn` is two of them, not one. Strip
    // them in a loop, otherwise only the outermost is removed and the
    // declaration is never recognised.
    const MODIFIERS: &[&str] = &[
        "pub ",
        "pub(crate) ",
        "pub(super) ",
        "pub(in crate::",
        "async ",
        "unsafe ",
        "extern ",
        "const ",
        "static ",
    ];
    let mut rest = line.trim_start();
    loop {
        let stripped = MODIFIERS
            .iter()
            .find_map(|modifier| rest.strip_prefix(modifier))
            .unwrap_or(rest);
        if stripped.len() == rest.len() {
            break;
        }
        rest = stripped.trim_start();
    }
    for keyword in ["fn ", "func ", "def ", "function ", "struct ", "enum ", "trait ", "class ", "impl "] {
        if let Some(after) = rest.strip_prefix(keyword) {
            let name: String = after
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            if !name.is_empty() && !DEFINITION_KEYWORDS.contains(&name.as_str()) {
                return Some(name);
            }
        }
    }
    None
}

/// The symbols a call site refers to.
fn called_names(line: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes: Vec<char> = line.chars().collect();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index].is_alphanumeric() || bytes[index] == '_' {
            let start = index;
            while index < bytes.len() && (bytes[index].is_alphanumeric() || bytes[index] == '_') {
                index += 1;
            }
            let name: String = bytes[start..index].iter().collect();
            let is_call = bytes.get(index) == Some(&'(');
            let preceded_by_path = start > 0 && bytes[start - 1] == '.';
            if is_call && !preceded_by_path && !name.is_empty() && name.chars().next().is_some_and(|c| c.is_alphabetic()) {
                names.push(name);
            }
        } else {
            index += 1;
        }
    }
    names
}

/// Analyse the workspace's source files.
///
/// Two passes: the declarations first, so a call site can be attributed to
/// the function that contains it; then the call sites.
fn analyze(workspace: &Path, security: &SecurityPolicy) -> Analysis {
    let files = collect_workspace_files(workspace, security, MAX_FILES);
    let files: Vec<PathBuf> = files.into_iter().filter(|file| is_source_path(file)).collect();
    let mut symbols: BTreeMap<String, Symbol> = BTreeMap::new();
    let mut per_file: BTreeMap<PathBuf, Vec<Symbol>> = BTreeMap::new();

    for file in &files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        for (index, line) in content.lines().enumerate() {
            let Some(name) = definition_name(line) else {
                continue;
            };
            let symbol = Symbol {
                file: file.clone(),
                line: index + 1,
                name: name.clone(),
            };
            symbols.entry(name.clone()).or_insert(symbol.clone());
            per_file.entry(file.clone()).or_default().push(symbol);
        }
    }

    let mut edges: Vec<Edge> = Vec::new();
    for file in &files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        let mut in_block_comment = false;
        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if in_block_comment {
                if trimmed.contains("*/") {
                    in_block_comment = false;
                }
                continue;
            }
            if trimmed.starts_with("/*") {
                in_block_comment = !trimmed.contains("*/");
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            let enclosing = per_file
                .get(file)
                .and_then(|symbols| {
                    symbols
                        .iter()
                        .filter(|symbol| symbol.line <= index + 1)
                        .max_by_key(|symbol| symbol.line)
                })
                .map(|symbol| symbol.name.clone())
                .unwrap_or_else(|| "<module>".to_string());
            for called in called_names(line) {
                edges.push(Edge {
                    from: enclosing.clone(),
                    to: called,
                    file: file.clone(),
                    line: index + 1,
                });
            }
        }
    }
    // Keep only edges whose target is a symbol the workspace defines: a call
    // to something the workspace never defines is not a blast-radius edge.
    let edges: Vec<Edge> = edges
        .into_iter()
        .filter(|edge| symbols.contains_key(&edge.to))
        .collect();
    Analysis { symbols, edges }
}

/// Who calls a symbol.
///
/// The result is the enclosing function of each call site, with the file and
/// line of the call, so "who calls this" is answerable without a second pass.
fn callers_of(analysis: &Analysis, symbol: &str) -> Vec<(String, PathBuf, usize)> {
    let mut callers: BTreeSet<(String, PathBuf, usize)> = BTreeSet::new();
    for edge in &analysis.edges {
        if edge.to == symbol {
            callers.insert((edge.from.clone(), edge.file.clone(), edge.line));
        }
    }
    callers.into_iter().collect()
}

/// What a symbol calls.
fn callees_of(analysis: &Analysis, symbol: &str) -> Vec<String> {
    let mut callees = BTreeSet::new();
    for edge in &analysis.edges {
        if edge.from == symbol {
            callees.insert(edge.to.clone());
        }
    }
    callees.into_iter().collect()
}

/// The file-level dependency edges of the workspace.
///
/// Two files are related when one mentions the other: a `mod` declaration, a
/// `use` of another module's path, or an import in another language.
fn file_edges(files: &[PathBuf]) -> BTreeMap<PathBuf, BTreeSet<PathBuf>> {
    let mut edges: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    let names: BTreeMap<String, &PathBuf> = files
        .iter()
        .filter_map(|file| {
            let stem = file.file_stem()?.to_str()?;
            Some((stem.to_string(), file))
        })
        .collect();
    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        let mut related = BTreeSet::new();
        for line in content.lines() {
            let trimmed = line.trim();
            for token in trimmed.split(|character: char| !character.is_alphanumeric() && character != '_') {
                if token.len() < 3 {
                    continue;
                }
                if let Some(other) = names.get(token)
                    && *other != file
                {
                    related.insert((*other).clone());
                }
            }
        }
        edges.insert(file.clone(), related);
    }
    edges
}

/// Who calls a symbol.
pub struct CallGraphTool {
    security: Arc<SecurityPolicy>,
}

impl CallGraphTool {
    /// Build the tool with the agent's security policy.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

/// The dependency graph of the workspace.
pub struct GraphTool {
    security: Arc<SecurityPolicy>,
}

impl GraphTool {
    /// Build the tool with the agent's security policy.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for CallGraphTool {
    fn name(&self) -> &str {
        "callgraph"
    }

    fn description(&self) -> &str {
        "Show what calls a function and what it calls. Answers the blast-radius question a content search cannot: name a symbol and you get its callers, its callees, and where each is defined."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "The function or method name to analyse, e.g. `parse_config`."
                },
                "direction": {
                    "type": "string",
                    "enum": ["callers", "callees", "both"],
                    "description": "Which side of the edge to report (default 'both')."
                },
                "depth": {
                    "type": "integer",
                    "description": "How many hops to walk (default 2)."
                }
            },
            "required": ["symbol"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let symbol = args
            .get("symbol")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow::Error::msg("Missing 'symbol' parameter"))?;
        let direction = args
            .get("direction")
            .and_then(|value| value.as_str())
            .unwrap_or("both");
        let depth = args
            .get("depth")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_DEPTH)
            .clamp(1, 8);

        let workspace = &self.security.workspace_dir;
        let analysis = analyze(workspace, &self.security);
        if !analysis.symbols.contains_key(symbol) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::text(format!(
                    "No symbol named `{symbol}` is defined in the workspace. The analysis is source-level: it sees definitions, not macros or dynamically dispatched names.\n"
                )),
                error: Some(format!("symbol not found: {symbol}")),
            });
        }

        let mut out = String::new();
        let definition = &analysis.symbols[symbol];
        out.push_str(&format!(
            "{} — defined at {}:{}\n",
            symbol,
            display_path(workspace, &definition.file),
            definition.line
        ));

        if direction == "callers" || direction == "both" {
            let mut frontier = vec![symbol.to_string()];
            let mut seen = BTreeSet::new();
            for hop in 1..=depth {
                let mut next = Vec::new();
                for name in &frontier {
                    for (caller, file, line) in callers_of(&analysis, name) {
                        if !seen.insert(caller.clone()) {
                            continue;
                        }
                        out.push_str(&format!(
                            "  caller (hop {hop}) {caller}  [{}:{}]\n",
                            display_path(workspace, &file),
                            line
                        ));
                        next.push(caller);
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next;
            }
            if seen.is_empty() {
                out.push_str("  no callers found in the workspace\n");
            }
        }

        if direction == "callees" || direction == "both" {
            let mut frontier = vec![symbol.to_string()];
            let mut seen = BTreeSet::new();
            for hop in 1..=depth {
                let mut next = Vec::new();
                for name in &frontier {
                    for callee in callees_of(&analysis, name) {
                        if !seen.insert(callee.clone()) {
                            continue;
                        }
                        out.push_str(&format!("  callee (hop {hop}) {callee}\n"));
                        next.push(callee);
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next;
            }
            if seen.is_empty() {
                out.push_str("  no callees found in the workspace\n");
            }
        }

        Ok(ToolResult {
            success: true,
            output: ToolOutput::text(out),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for GraphTool {
    fn name(&self) -> &str {
        "graph"
    }

    fn description(&self) -> &str {
        "Show the dependency graph of the workspace at file granularity: which files depend on which. Use it to size a change before you make it, or to find the files that own a piece of behaviour."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Report the neighbourhood of this file only. Defaults to the whole workspace."
                },
                "max_nodes": {
                    "type": "integer",
                    "description": "Maximum number of files to report (default 60)."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let max_nodes = args
            .get("max_nodes")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(60)
            .clamp(1, 500);
        let workspace = &self.security.workspace_dir;
        let files = collect_workspace_files(workspace, &self.security, MAX_FILES);
        let sources: Vec<PathBuf> = files.into_iter().filter(|file| is_source_path(file)).collect();
        let edges = file_edges(&sources);

        let focus = args
            .get("file")
            .and_then(|value| value.as_str())
            .map(|path| self.security.resolve_tool_path(path));
        if let Some(focus) = &focus {
            let Ok(resolved) = std::fs::canonicalize(focus) else {
                anyhow::bail!("cannot read {}", focus.display());
            };
            if !self.security.is_resolved_path_readable(&resolved) {
                anyhow::bail!("Path not allowed by security policy");
            }
        }

        let mut out = format!(
            "{} source file(s), {} dependency edge(s).\n",
            sources.len(),
            edges.values().map(BTreeSet::len).sum::<usize>()
        );
        let mut reported = 0usize;
        for (file, related) in &edges {
            if let Some(focus) = &focus {
                let focus_resolved = std::fs::canonicalize(focus).ok();
                let related_to_focus = focus_resolved
                    .as_ref()
                    .is_some_and(|focus| focus == file || related.contains(focus));
                if !related_to_focus {
                    continue;
                }
            }
            if reported >= max_nodes {
                out.push_str("… truncated\n");
                break;
            }
            reported += 1;
            let name = display_path(workspace, file);
            if related.is_empty() {
                out.push_str(&format!("{name}\n"));
                continue;
            }
            let mut names: Vec<String> = related
                .iter()
                .map(|other| display_path(workspace, other))
                .collect();
            names.sort();
            out.push_str(&format!("{name} → {}\n", names.join(", ")));
        }
        Ok(ToolResult {
            success: true,
            output: ToolOutput::text(out),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_definition_is_recognised() {
        assert_eq!(definition_name("fn parse_config() {"), Some("parse_config".into()));
        assert_eq!(definition_name("    pub fn read(&self) {"), Some("read".into()));
        assert_eq!(
            definition_name("pub(crate) async fn load() {"),
            Some("load".into())
        );
        assert_eq!(definition_name("struct Foo {"), Some("Foo".into()));
    }

    #[test]
    fn a_call_is_not_a_definition() {
        assert_eq!(definition_name("    let x = compute(1);"), None);
        assert_eq!(definition_name("if (ready()) {"), None);
    }

    #[test]
    fn called_names_skip_method_calls_and_words() {
        assert_eq!(called_names("let x = compute(1);"), vec!["compute"]);
        assert_eq!(called_names("self.parse();"), Vec::<String>::new());
        assert_eq!(called_names("let total = total + 1;"), Vec::<String>::new());
    }

    #[test]
    fn callers_and_callees_are_read_off_the_edges() {
        let mut symbols = BTreeMap::new();
        for name in ["a", "b", "c"] {
            symbols.insert(
                name.to_string(),
                Symbol {
                    file: PathBuf::from("/w/a.rs"),
                    line: 1,
                    name: name.into(),
                },
            );
        }
        let analysis = Analysis {
            symbols,
            edges: vec![
                Edge {
                    from: "a".into(),
                    to: "b".into(),
                    file: PathBuf::from("/w/a.rs"),
                    line: 3,
                },
                Edge {
                    from: "c".into(),
                    to: "b".into(),
                    file: PathBuf::from("/w/a.rs"),
                    line: 7,
                },
            ],
        };
        assert_eq!(
            callers_of(&analysis, "b"),
            vec![
                ("a".to_string(), PathBuf::from("/w/a.rs"), 3),
                ("c".to_string(), PathBuf::from("/w/a.rs"), 7),
            ]
        );
        assert_eq!(callees_of(&analysis, "a"), vec!["b"]);
    }
}
