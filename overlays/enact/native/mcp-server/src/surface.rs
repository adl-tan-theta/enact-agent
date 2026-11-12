//! The tool surface an MCP host sees.
//!
//! Advertising every tool schema eagerly would spend the host's context on
//! tools the model will never call. The surface therefore publishes two
//! tools — `mcp_search` and `mcp_call` — and lets the model pull a schema
//! only when it needs one. The budget mirrors what jcode ships: at most 32
//! tool references per search under roughly a 6000-token ceiling.
//!
//! The surface also carries the consolidation: a tool that a canonical
//! family now covers keeps working, but it is no longer advertised. The
//! family is the name the model should learn, and the canonical registry
//! the bridge publishes lists only the families. A host that still calls a
//! legacy name gets the result it expects, plus a one-time notice naming
//! the canonical form.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Value, json};
use zeroclaw_api::tool::Tool;
use zeroclaw_tools::mcp_protocol::McpToolDef;

/// Default number of tool references returned by one search.
pub const DEFAULT_SEARCH_LIMIT: usize = 8;
/// Hard cap on tool references per search.
pub const MAX_SEARCH_TOOL_REFERENCES: usize = 32;
/// Approximate token ceiling for one search result.
pub const SEARCH_TOKEN_BUDGET: usize = 6000;
/// Rough characters-per-token ratio used for the budget estimate.
pub const CHARS_PER_TOKEN: usize = 4;

/// Words that carry no signal in a tool query.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "in", "on", "for", "with", "and", "or", "is", "it", "its", "my",
    "me", "do", "can", "how", "what", "when", "where", "which", "that", "this", "by", "be", "as",
    "at", "from", "any", "some", "use", "using", "get", "make", "run",
];

/// A tool the host can call.
pub struct ToolSurface {
    tools: Vec<Arc<dyn Tool>>,
    index: HashMap<String, usize>,
    /// The tools a canonical family now covers: callable, not advertised.
    legacy: HashSet<String>,
}

impl ToolSurface {
    /// Build a surface over an ordered tool registry.
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        let mut index = HashMap::with_capacity(tools.len());
        let mut legacy = HashSet::new();
        for (position, tool) in tools.iter().enumerate() {
            index.entry(tool.name().to_string()).or_insert(position);
            if zeroclaw_runtime::tools::families::is_legacy_member(tool.name()) {
                legacy.insert(tool.name().to_string());
            }
        }
        Self {
            tools,
            index,
            legacy,
        }
    }

    /// An empty surface, for the transport tests.
    ///
    /// The production surface always comes from the registry.
    #[cfg(test)]
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// How many tools the surface advertises.
    ///
    /// The count excludes the legacy names a canonical family covers: they
    /// still work, but the family is what a host is offered.
    pub fn len(&self) -> usize {
        self.tools.len() - self.legacy.len()
    }

    /// Whether the surface has no tools.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Look up a tool by its exact name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.index
            .get(name)
            .and_then(|position| self.tools.get(*position))
    }

    /// Every advertised tool as an MCP descriptor, for hosts that want a
    /// flat surface.
    pub fn descriptors(&self) -> Vec<McpToolDef> {
        self.tools
            .iter()
            .filter(|tool| !self.legacy.contains(tool.name()))
            .map(|tool| McpToolDef {
                name: tool.name().to_string(),
                description: Some(tool.description().to_string()),
                input_schema: tool.parameters_schema(),
            })
            .collect()
    }

    /// Search the registry by intent.
    pub fn search(&self, query: &str, limit: usize) -> Value {
        let words = tokenize(query);
        let query_lower = query.trim().to_lowercase();
        let limit = limit.clamp(1, MAX_SEARCH_TOOL_REFERENCES);

        let list_everything = query.trim().is_empty();
        let mut scored: Vec<(i64, Vec<String>, &Arc<dyn Tool>)> = Vec::new();
        for tool in &self.tools {
            // A legacy name a canonical family covers is not offered: the
            // family is what the model should learn.
            if self.legacy.contains(tool.name()) {
                continue;
            }
            if words.is_empty() {
                // An empty query lists the registry. A query made only of
                // stop words matches nothing; it is not a request for all.
                if list_everything {
                    scored.push((0, Vec::new(), tool));
                }
                continue;
            }
            let (score, matched) = score_tool(tool, &query_lower, &words);
            if score > 0 {
                scored.push((score, matched, tool));
            }
        }
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.2.name().cmp(right.2.name()))
        });

        let mut results = Vec::new();
        let mut used_tokens = 0usize;
        for (score, matched, tool) in scored.iter().take(limit) {
            let entry = json!({
                "name": tool.name(),
                "description": tool.description(),
                "parameters": parameter_names(tool),
                "score": score,
            });
            let cost = estimate_tokens(&entry);
            if used_tokens + cost > SEARCH_TOKEN_BUDGET && !results.is_empty() {
                break;
            }
            used_tokens += cost;
            let mut entry = entry;
            if let Some(object) = entry.as_object_mut() {
                object.insert("matched_on".to_string(), json!(matched));
            }
            results.push(entry);
        }

        json!({
            "query": query,
            "results": results,
            "matched": scored.len(),
            "truncated": scored.len() > results.len(),
            "token_estimate": used_tokens,
            "registry_tools": self.len(),
        })
    }

    /// Full detail for one tool, including its parameter schema.
    pub fn detail(&self, name: &str) -> Option<Value> {
        let tool = self.get(name)?;
        Some(json!({
            "name": tool.name(),
            "description": tool.description(),
            "parameters": tool.parameters_schema(),
            "output_schema": tool.output_schema(),
        }))
    }

    /// Execute a tool by name.
    pub async fn call(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let tool = self.get(name).ok_or_else(|| {
            format!("unknown tool {name:?}; use mcp_search to find the right name")
        })?;
        match tool.execute(arguments).await {
            Ok(result) => {
                let mut payload = json!({
                    "success": result.success,
                    "output": result.output.as_str(),
                    "error": result.error,
                });
                if let Some(data) = result.output.data() {
                    payload["data"] = data.clone();
                }
                // A legacy name still works. The notice tells the model which
                // canonical family replaces it, once, beside the result.
                if self.legacy.contains(name) {
                    let mut data = payload.get("data").cloned();
                    zeroclaw_runtime::tools::families::attach_deprecation_notice(name, &mut data);
                    if let Some(data) = data {
                        payload["data"] = data;
                    }
                }
                Ok(payload)
            }
            Err(error) => Err(format!("{error:#}")),
        }
    }
}

/// Split a query into meaningful tokens.
fn tokenize(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .filter(|token| token.len() > 1 && !STOP_WORDS.contains(&token.as_str()))
        .collect()
}

/// The declared parameter names of a tool.
fn parameter_names(tool: &Arc<dyn Tool>) -> Vec<String> {
    tool.parameters_schema()
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

/// Rough token cost of a JSON value.
fn estimate_tokens(value: &Value) -> usize {
    let rendered = value.to_string();
    rendered.len() / CHARS_PER_TOKEN + 8
}

/// Score one tool against a query.
///
/// A name hit beats a description hit, a description hit beats a parameter
/// hit, and a tool whose name contains every query word outranks a tool that
/// merely mentions them.
fn score_tool(tool: &Arc<dyn Tool>, query: &str, words: &[String]) -> (i64, Vec<String>) {
    let name = tool.name().to_lowercase();
    let description = tool.description().to_lowercase();
    let parameters = parameter_names(tool);
    let parameters_lower: Vec<String> = parameters.iter().map(|p| p.to_lowercase()).collect();

    let mut score = 0i64;
    let mut matched: Vec<String> = Vec::new();

    if name == query {
        score += 100;
        matched.push("name==".to_string());
    } else if name.starts_with(query) {
        score += 60;
        matched.push("name^".to_string());
    } else if name.contains(query) {
        score += 40;
        matched.push("name~".to_string());
    }

    let mut name_hits = 0usize;
    for word in words {
        if name.contains(word.as_str()) {
            score += 20;
            name_hits += 1;
            matched.push(format!("name:{word}"));
        }
        if description.contains(word.as_str()) {
            score += 5;
            matched.push(format!("desc:{word}"));
        }
        if parameters_lower.iter().any(|p| p.contains(word.as_str())) {
            score += 6;
            matched.push(format!("param:{word}"));
        }
    }
    if !words.is_empty() && name_hits == words.len() {
        score += 25;
        matched.push("name:all".to_string());
    }

    (score, matched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::attribution::{Role, ToolProvenance};

    struct FakeTool {
        name: &'static str,
        description: &'static str,
        parameters: Value,
    }

    impl zeroclaw_api::attribution::Attributable for FakeTool {
        fn role(&self) -> Role {
            Role::Agent
        }
        fn alias(&self) -> &str {
            "fake"
        }
        fn tool_provenance(&self) -> ToolProvenance {
            ToolProvenance::Extension
        }
    }

    #[async_trait::async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.description
        }
        fn parameters_schema(&self) -> Value {
            self.parameters.clone()
        }
        async fn execute(&self, _args: Value) -> anyhow::Result<zeroclaw_api::tool::ToolResult> {
            Ok(zeroclaw_api::tool::ToolResult::ok("executed"))
        }
    }

    fn tool(name: &'static str, description: &'static str) -> Arc<dyn Tool> {
        Arc::new(FakeTool {
            name,
            description,
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
            }),
        })
    }

    fn surface() -> ToolSurface {
        ToolSurface::new(vec![
            tool("file_read", "Read a workspace file with line numbers."),
            tool(
                "shell",
                "Execute a shell command in the workspace directory.",
            ),
            tool(
                "web_search_tool",
                "Search the web and return results with citations.",
            ),
        ])
    }

    #[test]
    fn search_prefers_the_intended_tool() {
        let result = surface().search("read a file", DEFAULT_SEARCH_LIMIT);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["name"], json!("file_read"));
    }

    #[test]
    fn search_finds_a_shell_tool() {
        let result = surface().search("execute a shell command", DEFAULT_SEARCH_LIMIT);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["name"], json!("shell"));
    }

    #[test]
    fn search_lists_every_tool_without_a_query() {
        let result = surface().search("", DEFAULT_SEARCH_LIMIT);
        assert_eq!(result["results"].as_array().unwrap().len(), 3);
        assert_eq!(result["matched"], json!(3));
    }

    #[test]
    fn stop_words_do_not_match_everything() {
        let result = surface().search("a the of", DEFAULT_SEARCH_LIMIT);
        assert_eq!(result["results"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn detail_returns_the_parameter_schema() {
        let detail = surface().detail("file_read").unwrap();
        assert_eq!(detail["name"], json!("file_read"));
        assert!(detail["parameters"]["properties"]["path"].is_object());
    }

    #[test]
    fn detail_of_an_unknown_tool_is_none() {
        assert!(surface().detail("nope").is_none());
    }

    #[test]
    fn descriptors_carry_the_input_schema() {
        let descriptors = surface().descriptors();
        assert_eq!(descriptors.len(), 3);
        assert_eq!(descriptors[0].input_schema["type"], json!("object"));
    }

    #[tokio::test]
    async fn call_executes_the_tool() {
        let outcome = surface().call("file_read", json!({})).await.unwrap();
        assert_eq!(outcome["success"], json!(true));
    }

    #[tokio::test]
    async fn call_reports_an_unknown_tool() {
        let error = surface().call("nope", json!({})).await.unwrap_err();
        assert!(error.contains("unknown tool"));
    }
}
