//! Read the MCP configuration the coding hosts already have.
//!
//! Codex CLI and Claude Code each keep their own list of MCP servers. When
//! Enact serves its own registry over MCP it must not silently disagree with
//! what the user configured, so the server discovers the host configuration
//! and applies the same precedence the hosts apply:
//!
//! 1. `~/.codex/config.toml` `[mcp_servers.*]` — imported once, lowest precedence
//! 2. `~/.claude.json` `mcpServers` — the user's Claude Code servers
//! 3. `<project>/.mcp.json` `mcpServers` — the project's servers, highest precedence
//!
//! The tie-break rule is the one that matters: a *runnable* server wins over a
//! lower-precedence broken one. A higher-precedence entry that carries neither
//! a command nor a URL cannot start, so the next runnable entry down the list
//! is used instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where a server entry was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpConfigSource {
    /// `~/.codex/config.toml`
    CodexConfigToml,
    /// `~/.claude.json`
    ClaudeUserConfig,
    /// `<project>/.mcp.json`
    ProjectMcpJson,
}

impl McpConfigSource {
    /// A human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            McpConfigSource::CodexConfigToml => "codex config.toml",
            McpConfigSource::ClaudeUserConfig => "claude user config",
            McpConfigSource::ProjectMcpJson => "project .mcp.json",
        }
    }
}

/// One MCP server the user has configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerEntry {
    /// The server name the host addresses it by.
    pub name: String,
    /// The executable, for a stdio server.
    pub command: Option<String>,
    /// Arguments for the executable.
    pub args: Vec<String>,
    /// The endpoint, for an HTTP server.
    pub url: Option<String>,
    /// Environment for the executable.
    pub env: BTreeMap<String, String>,
    /// Where it was found.
    pub source: McpConfigSource,
}

impl McpServerEntry {
    /// Whether this entry can actually be started.
    pub fn is_runnable(&self) -> bool {
        self.url.is_some() || self.command.as_ref().is_some_and(|c| !c.trim().is_empty())
    }
}

/// The merged view of every host MCP configuration.
#[derive(Debug, Clone, Default)]
pub struct McpConfigDiscovery {
    /// The servers, highest precedence first.
    pub servers: Vec<McpServerEntry>,
    /// Names that appeared more than once.
    pub shadowed: Vec<String>,
}

impl McpConfigDiscovery {
    /// Look up one server by name.
    pub fn get(&self, name: &str) -> Option<&McpServerEntry> {
        self.servers.iter().find(|entry| entry.name == name)
    }

    /// The names of every discovered server.
    pub fn names(&self) -> Vec<String> {
        self.servers
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
}

/// Discover the MCP servers configured for a project.
///
/// `home` is the user's home directory; it is a parameter so the discovery can
/// be exercised against a fixture instead of the real machine.
pub fn discover(project_dir: &Path, home: &Path) -> McpConfigDiscovery {
    let mut candidates: Vec<McpServerEntry> = Vec::new();

    // Lowest precedence first: the one-time Codex import.
    candidates.extend(read_codex_config(&home.join(".codex").join("config.toml")));

    // The user's Claude Code servers.
    candidates.extend(read_claude_user_config(&home.join(".claude.json")));

    // Highest precedence last: the project file.
    candidates.extend(read_project_mcp_json(&project_mcp_json_path(project_dir)));

    merge_by_precedence(candidates)
}

/// Merge entries by precedence, letting a runnable server win.
fn merge_by_precedence(candidates: Vec<McpServerEntry>) -> McpConfigDiscovery {
    let mut ordered = candidates;
    // Highest source first, then by name so the result is deterministic.
    ordered.sort_by(|left, right| {
        right
            .source
            .cmp(&left.source)
            .then_with(|| left.name.cmp(&right.name))
    });

    let mut discovery = McpConfigDiscovery::default();
    for entry in ordered {
        match discovery
            .servers
            .iter_mut()
            .find(|kept| kept.name == entry.name)
        {
            Some(kept) => {
                // A runnable entry always beats a broken one, whatever the
                // precedence; otherwise the higher-precedence entry stands.
                if kept.is_runnable() && !entry.is_runnable() {
                    continue;
                }
                if !kept.is_runnable() && entry.is_runnable() {
                    *kept = entry;
                    continue;
                }
                if !discovery.shadowed.contains(&entry.name) {
                    discovery.shadowed.push(entry.name.clone());
                }
            }
            None => discovery.servers.push(entry),
        }
    }
    discovery
        .servers
        .sort_by(|left, right| left.name.cmp(&right.name));
    discovery
}

/// Read `[mcp_servers.*]` from a Codex config.
fn read_codex_config(path: &Path) -> Vec<McpServerEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let Some(servers) = value.get("mcp_servers").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for (name, server) in servers {
        let Some(table) = server.as_table() else {
            continue;
        };
        entries.push(McpServerEntry {
            name: name.clone(),
            command: table
                .get("command")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
            args: table
                .get("args")
                .and_then(toml::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            url: table
                .get("url")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
            env: read_toml_env(table),
            source: McpConfigSource::CodexConfigToml,
        });
    }
    entries
}

/// Read the `env` / `env_vars` shape Codex uses.
fn read_toml_env(table: &toml::Table) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(nested) = table.get("env").and_then(toml::Value::as_table) {
        for (key, value) in nested {
            if let Some(value) = value.as_str() {
                env.insert(key.clone(), value.to_string());
            }
        }
    }
    if let Some(names) = table.get("env_vars").and_then(toml::Value::as_array) {
        for name in names.iter().filter_map(toml::Value::as_str) {
            env.entry(name.to_string()).or_default();
        }
    }
    env
}

/// Read `mcpServers` from `~/.claude.json`.
fn read_claude_user_config(path: &Path) -> Vec<McpServerEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    read_json_servers(&value, McpConfigSource::ClaudeUserConfig)
}

/// Read `mcpServers` from a `.mcp.json` file.
fn read_project_mcp_json(path: &Path) -> Vec<McpServerEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    read_json_servers(&value, McpConfigSource::ProjectMcpJson)
}

/// Extract the `mcpServers` object from a JSON document.
fn read_json_servers(value: &Value, source: McpConfigSource) -> Vec<McpServerEntry> {
    let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else {
        return Vec::new();
    };
    servers
        .iter()
        .filter_map(|(name, server)| {
            let table = server.as_object()?;
            Some(McpServerEntry {
                name: name.clone(),
                command: table
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                args: table
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                url: table.get("url").and_then(Value::as_str).map(str::to_string),
                env: table
                    .get("env")
                    .and_then(Value::as_object)
                    .map(|env| {
                        env.iter()
                            .filter_map(|(key, value)| {
                                value.as_str().map(|value| (key.clone(), value.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                source,
            })
        })
        .collect()
}

/// Where the hosts keep their project-level MCP file.
pub fn project_mcp_json_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".mcp.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn fixture() -> (tempdir::TempDir, PathBuf, PathBuf) {
        let dir = tempdir::TempDir::new("mcp-discovery");
        let project = dir.path().join("project");
        let home = dir.path().join("home");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&home).unwrap();
        (dir, project, home)
    }

    /// A tiny temporary-directory helper; the crate takes no dev-dependency
    /// for a scope this small.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new(prefix: &str) -> Self {
                let id = COUNTER.fetch_add(1, Ordering::Relaxed);
                let path =
                    std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn reads_the_project_file() {
        let (_guard, project, home) = fixture();
        write(
            &project.join(".mcp.json"),
            r#"{"mcpServers": {"enact": {"command": "python3", "args": ["serve.py"]}}}"#,
        );
        let found = discover(&project, &home);
        let entry = found.get("enact").expect("enact server");
        assert_eq!(entry.command.as_deref(), Some("python3"));
        assert_eq!(entry.args, vec!["serve.py".to_string()]);
        assert_eq!(entry.source, McpConfigSource::ProjectMcpJson);
    }

    #[test]
    fn reads_the_codex_config() {
        let (_guard, project, home) = fixture();
        write(
            &home.join(".codex").join("config.toml"),
            "[mcp_servers.enact]\ncommand = \"python3\"\nargs = [\"serve.py\"]\n",
        );
        let found = discover(&project, &home);
        let entry = found.get("enact").expect("enact server");
        assert_eq!(entry.source, McpConfigSource::CodexConfigToml);
        assert_eq!(entry.command.as_deref(), Some("python3"));
    }

    #[test]
    fn reads_the_claude_user_config() {
        let (_guard, project, home) = fixture();
        write(
            &home.join(".claude.json"),
            r#"{"mcpServers": {"remote": {"url": "https://example.com/mcp"}}}"#,
        );
        let found = discover(&project, &home);
        let entry = found.get("remote").expect("remote server");
        assert_eq!(entry.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(entry.source, McpConfigSource::ClaudeUserConfig);
    }

    #[test]
    fn a_runnable_server_wins_over_a_broken_one() {
        let (_guard, project, home) = fixture();
        // Codex has a runnable entry; the project file has a broken one.
        write(
            &home.join(".codex").join("config.toml"),
            "[mcp_servers.enact]\ncommand = \"python3\"\n",
        );
        write(
            &project.join(".mcp.json"),
            r#"{"mcpServers": {"enact": {"description": "no command"}}}"#,
        );
        let found = discover(&project, &home);
        let entry = found.get("enact").expect("enact server");
        assert!(entry.is_runnable());
        assert_eq!(entry.source, McpConfigSource::CodexConfigToml);
    }

    #[test]
    fn a_higher_precedence_runnable_server_wins() {
        let (_guard, project, home) = fixture();
        write(
            &home.join(".codex").join("config.toml"),
            "[mcp_servers.enact]\ncommand = \"codex-server\"\n",
        );
        write(
            &project.join(".mcp.json"),
            r#"{"mcpServers": {"enact": {"command": "project-server"}}}"#,
        );
        let found = discover(&project, &home);
        let entry = found.get("enact").expect("enact server");
        assert_eq!(entry.command.as_deref(), Some("project-server"));
        assert_eq!(entry.source, McpConfigSource::ProjectMcpJson);
    }

    #[test]
    fn a_missing_config_is_not_an_error() {
        let (_guard, project, home) = fixture();
        let found = discover(&project, &home);
        assert!(found.servers.is_empty());
    }
}
