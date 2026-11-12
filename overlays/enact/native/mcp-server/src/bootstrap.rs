//! Bootstrap the Enact tool registry for an MCP host.
//!
//! The registry is built exactly the way a live agent turn builds it: the
//! configured agent's risk profile decides the security policy, the runtime
//! decides the execution substrate, and the assembled registry is filtered by
//! `allowed_tools` / `excluded_tools` before a host ever sees it.
//!
//! The raw `AllToolsResult` is deliberately NOT exposed. Its tool vectors are
//! unfiltered by contract — every production caller routes them through
//! `ScopedToolRegistry::assemble` first, and so does this one.
//!
//! When no agent is configured the server still has to work: a host may point
//! at it before the user has set anything up. It then falls back to the
//! default risk profile — supervised, workspace-only, high-risk commands
//! blocked — with the requested workspace as the jail root, and says so.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{Config, RiskProfileConfig};

use crate::mcp_server::surface::ToolSurface;

/// The alias used when the configuration declares no agent.
pub const DEFAULT_AGENT_ALIAS: &str = "default";

/// How to build the registry.
#[derive(Debug, Clone, Default)]
pub struct BootstrapOptions {
    /// The agent whose policy governs the exposed tools. Defaults to the first
    /// configured agent.
    pub agent_alias: Option<String>,
    /// Override the workspace root. Defaults to the agent's workspace.
    pub workspace: Option<PathBuf>,
    /// Connect the agent's configured MCP servers so their tools are part of
    /// the surface.
    pub connect_mcp: bool,
}

/// A ready-to-serve tool surface.
pub struct Bootstrapped {
    /// The policy-filtered tools.
    pub surface: ToolSurface,
    /// The agent whose policy produced the surface.
    pub agent_alias: String,
    /// The workspace root the tools are jailed to.
    pub workspace: PathBuf,
    /// Whether the agent's own policy was used, or the conservative default.
    pub used_configured_agent: bool,
}

/// Build the policy-filtered tool surface.
pub async fn build_tool_surface(options: &BootstrapOptions) -> anyhow::Result<Bootstrapped> {
    let config = Config::load_or_init()
        .await
        .context("failed to load the Enact configuration")?;

    let agent_alias = match options.agent_alias.clone() {
        Some(alias) => alias,
        None => config
            .agents
            .keys()
            .min()
            .cloned()
            .unwrap_or_else(|| DEFAULT_AGENT_ALIAS.to_string()),
    };
    let configured_agent = config.agents.contains_key(&agent_alias);

    let workspace = match options.workspace.clone() {
        Some(workspace) => workspace,
        None if configured_agent => config.agent_workspace_dir(&agent_alias),
        None => std::env::current_dir().context("failed to resolve the current directory")?,
    };

    // The default profile backs the fallback so its borrow outlives the branch.
    let default_profile = RiskProfileConfig::default();
    let (security, risk_profile) = match config.risk_profile_for_agent(&agent_alias) {
        Some(profile) => {
            let mut policy =
                SecurityPolicy::for_agent(&config, &agent_alias).with_context(|| {
                    format!("agents.{agent_alias}: failed to build the security policy")
                })?;
            // The requested workspace becomes the jail root, exactly as a live
            // session cwd does for an agent turn.
            if options.workspace.is_some() {
                policy.workspace_dir = workspace.clone();
                if !policy.allowed_roots.contains(&workspace) {
                    policy.allowed_roots.push(workspace.clone());
                }
            }
            (Arc::new(policy), profile)
        }
        None => {
            let policy = SecurityPolicy::from_risk_profile(&default_profile, &workspace);
            (Arc::new(policy), &default_profile)
        }
    };

    let runtime: Arc<dyn RuntimeAdapter> = Arc::from(
        zeroclaw_config::platform::create_runtime(&config.runtime)
            .context("failed to create the runtime adapter")?,
    );

    let provider_api_key = config
        .resolved_model_provider_for_agent(&agent_alias)
        .and_then(|(_, _, entry)| entry.api_key.as_deref());

    let memory: Arc<dyn Memory> = if configured_agent {
        zeroclaw_memory::create_memory_for_agent(&config, &agent_alias, provider_api_key)
            .await
            .with_context(|| format!("agents.{agent_alias}: failed to create the memory backend"))?
    } else {
        match zeroclaw_memory::create_memory(&config.memory, &workspace, provider_api_key) {
            Ok(memory) => Arc::from(memory),
            Err(_) => Arc::from(zeroclaw_memory::NoneMemory::new(&agent_alias)),
        }
    };

    let built = zeroclaw_runtime::tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        risk_profile,
        &agent_alias,
        Arc::clone(&runtime),
        Arc::clone(&memory),
        config.composio.api_key.as_deref(),
        Some(config.composio.entity_id.as_str()),
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &workspace,
        &config.agents,
        provider_api_key,
        &config,
        None,
        false,
        None,
        None,
        None,
        None,
    )
    .context("failed to build the built-in tool registry")?;

    // The policy gate. `assemble` applies `allowed_tools` / `excluded_tools`
    // and the MCP access policy; skipping it would hand a host tools the
    // agent is not allowed to use.
    let assembled = zeroclaw_runtime::tools::scoped::ScopedToolRegistry::assemble(
        zeroclaw_runtime::tools::scoped::ScopedAssembly {
            config: &config,
            agent_alias: &agent_alias,
            security: &security,
            built,
            skills: &[],
            runtime: Arc::clone(&runtime),
            caller_allowed: None,
            connect_mcp: options.connect_mcp,
            connect_peripherals: false,
            exclude_memory: false,
            acp_delivery: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        },
    )
    .await;

    let tools: Vec<Arc<dyn Tool>> = assembled
        .registry
        .into_inner()
        .into_iter()
        .map(Arc::from)
        .collect();

    if tools.is_empty() {
        anyhow::bail!(
            "no tools are available for agent {agent_alias:?}; check allowed_tools / excluded_tools"
        );
    }

    Ok(Bootstrapped {
        surface: ToolSurface::new(tools),
        agent_alias,
        workspace,
        used_configured_agent: configured_agent,
    })
}
