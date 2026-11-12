//! The `enact-agent mcp` command surface.

use std::path::PathBuf;

use clap::Subcommand;

use crate::mcp_server::bootstrap::{BootstrapOptions, build_tool_surface};
use crate::mcp_server::server::{Expose, McpServer, serve_stdio};

/// Subcommands of `enact-agent mcp`.
#[derive(Subcommand, Debug, Clone)]
pub enum McpCommands {
    /// Serve the built-in tool registry to MCP hosts (Codex CLI, Claude Code).
    ///
    /// The registry is served over stdio with newline-delimited JSON-RPC 2.0.
    /// By default the host sees `mcp_search` and `mcp_call`; `--expose-full`
    /// advertises every tool instead.
    Serve {
        /// Agent whose policy governs the exposed tools.
        #[arg(long)]
        agent: Option<String>,

        /// Workspace root the tools are jailed to. Defaults to the agent's
        /// configured workspace.
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Advertise every tool instead of the search + call surface.
        #[arg(long)]
        expose_full: bool,

        /// Connect the agent's configured MCP servers and include their tools.
        #[arg(long)]
        connect_mcp: bool,

        /// Serve over HTTP instead of stdio.
        ///
        /// HTTP serves one JSON-RPC message per POST to `/`, and answers
        /// `GET /health` with `ok`. The server binds the loopback address
        /// unless `--http-host` says otherwise: it runs the agent's own
        /// tools, so it must not be reachable from the network by accident.
        #[arg(long)]
        http: bool,

        /// The port the HTTP transport binds.
        #[arg(long, default_value_t = crate::mcp_server::http::DEFAULT_HTTP_PORT)]
        http_port: u16,

        /// The address the HTTP transport binds.
        #[arg(long, default_value = "127.0.0.1")]
        http_host: String,
    },

    /// List the MCP servers the coding hosts already have configured.
    ///
    /// Reads the project's `.mcp.json`, the user's `~/.claude.json` and the
    /// Codex `~/.codex/config.toml`, and reports them with the precedence the
    /// hosts apply.
    Hosts {
        /// Project directory to read `.mcp.json` from.
        #[arg(long)]
        project: Option<PathBuf>,

        /// Print the raw discovery result as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Run an `mcp` subcommand.
pub async fn run(command: McpCommands) -> anyhow::Result<()> {
    match command {
        McpCommands::Serve {
            agent,
            workspace,
            expose_full,
            connect_mcp,
            http,
            http_port,
            http_host,
        } => {
            let options = BootstrapOptions {
                agent_alias: agent,
                workspace,
                connect_mcp,
            };
            let bootstrapped = build_tool_surface(&options).await?;
            let expose = if expose_full {
                Expose::Full
            } else {
                Expose::Minimal
            };
            // stdout belongs to the protocol. Progress goes to stderr.
            eprintln!(
                "[enact-agent] serving {} tools for agent {} (workspace {})",
                bootstrapped.surface.len(),
                bootstrapped.agent_alias,
                bootstrapped.workspace.display()
            );
            if bootstrapped.surface.is_empty() {
                eprintln!(
                    "[enact-agent] the agent's policy exposes no tools; \
                     the host will see an empty registry"
                );
            }
            if !bootstrapped.used_configured_agent {
                eprintln!(
                    "[enact-agent] no configured agent supplied a risk profile; \
                     using the default supervised, workspace-only policy"
                );
            }
            let server = McpServer::new(bootstrapped.surface, expose);
            if http {
                crate::mcp_server::http::serve_http(server, &http_host, http_port).await
            } else {
                serve_stdio(server).await
            }
        }
        McpCommands::Hosts { project, json } => {
            let project_dir = project
                .map(|path| path.canonicalize().unwrap_or(path))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            let discovery = crate::mcp_server::config_discovery::discover(&project_dir, &home);
            if json {
                let payload = serde_json::json!({
                    "project": project_dir.display().to_string(),
                    "servers": discovery.servers.iter().map(|entry| serde_json::json!({
                        "name": entry.name,
                        "command": entry.command,
                        "args": entry.args,
                        "url": entry.url,
                        "source": entry.source.label(),
                        "runnable": entry.is_runnable(),
                    })).collect::<Vec<_>>(),
                    "shadowed": discovery.shadowed,
                    "enact_registered": discovery.get("enact").is_some(),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("MCP servers configured for the coding hosts:");
                for entry in &discovery.servers {
                    let target = entry
                        .url
                        .clone()
                        .or_else(|| {
                            entry
                                .command
                                .clone()
                                .map(|command| format!("{command} {}", entry.args.join(" ")))
                        })
                        .unwrap_or_else(|| "(not runnable)".to_string());
                    println!("  {:<24} {:<22} {target}", entry.name, entry.source.label());
                }
                if discovery.servers.is_empty() {
                    println!("  (none found)");
                } else {
                    println!(
                        "  {} server(s): {}",
                        discovery.names().len(),
                        discovery.names().join(", ")
                    );
                    if discovery.get("enact").is_none() {
                        println!(
                            "  note: no server named 'enact' is registered; \
                             run `enact-agent mcp serve` and add it to the host"
                        );
                    }
                }
            }
            Ok(())
        }
    }
}
