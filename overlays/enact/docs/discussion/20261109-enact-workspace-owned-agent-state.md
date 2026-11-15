# Enact Agent state belongs to the workspace

## Decision

Use a workspace-first layout: a workspace owns its agents. Standalone Enact Agent uses `$ENACT_HOME/workspace/agents/<agent-alias>/` by default. Runtime config, databases, and daemon/process state stay under `$ENACT_HOME/agent/`. Shared agent assets live at `$ENACT_HOME/shared/`; agent-owned files live below `$ENACT_HOME/workspace/agents/`.

```text
$ENACT_HOME/
  agent/                         # config.toml, databases, daemon/process state
  shared/                        # assets shared by local agents
  workspace/
    agents/<agent-alias>/        # bootstrap files and local agent workspace content
```

When `ENACT_HOME` is unset, the native default is `~/.enact`. `ENACT_AGENT_CONFIG_DIR` remains the explicit config override.

## Enact-hosted sessions

Enact's Agent Profile is the durable authority for identity and instructions in an Enact workspace. The Enact harness materializes the admitted content into a private per-session workspace projection because the daemon needs local files. Session credentials, socket, run state, and projection remain scoped to that session. The standalone `$ENACT_HOME/workspace/agents/<alias>` default does not become a second profile store for Enact-hosted sessions.

Enact's local storage layout already identifies workspace files with tenant, organization, account, and workspace IDs. Do not substitute a global `workspace/` path for that multi-workspace storage boundary. The standalone CLI has one local workspace by default; the Enact host supplies its own private session projection. The inspected local Enact storage has workspace directories separate from per-session sandbox directories; the sample durable workspace directory was empty, while session sandboxes each had their own `workspace/` and `sessions/` areas.

## Implementation

`overlays/enact/build.py` now applies exact, fail-closed transformations to the generated source projection:

- Config and native runtime state default to `$ENACT_HOME/agent/` (or `~/.enact/agent/`).
- The standalone per-agent workspace defaults to `$ENACT_HOME/workspace/agents/<alias>/` (or `~/.enact/workspace/agents/<alias>/`). `install_root_dir()` resolves to `$ENACT_HOME` for the default config so workspace export still walks the protected `workspace/` subtree; an explicit config directory remains its own root.
- Explicit `ENACT_AGENT_CONFIG_DIR` and `[agents.<alias>.workspace.path]` overrides remain authoritative.
- Upstream crate/module names and source files remain unchanged; generated changes belong under ignored `target/enact/source/`.

No existing `~/.zeroclaw` data is moved or deleted by this change. A rebuilt Enact Agent uses the new defaults; existing data remains available at its current path until separately handled.

## Boundaries

| State | Authority | Location |
|---|---|---|
| Enact Agent profile and durable instructions | Enact workspace | Enact's workspace-scoped profile store |
| Standalone bootstrap files and local workspace content | Standalone local workspace | `$ENACT_HOME/workspace/agents/<alias>/` |
| Hosted-session config, credentials, socket, run state, and file projection | One admitted Enact session | Existing private session runtime directory |
| Agent process databases and daemon state | Local Enact Agent runtime | `$ENACT_HOME/agent/` |

## Source references

- Enact workspace storage addresses: [`file-operations-storage-addresses.ts`](/Users/tarunrana/workspace/Enact/packages/file-operations/src/storage/file-operations-storage-addresses.ts)
- Enact physical workspace paths: [`host-paths.ts`](/Users/tarunrana/workspace/Enact/packages/storage/src/host-paths.ts)
- Enact Agent session projection: [`enact-agent-session.ts`](/Users/tarunrana/workspace/Enact/packages/harnesses/src/enact/enact-agent-session.ts)
- Enact Agent config projection: [`enact-agent-config.ts`](/Users/tarunrana/workspace/Enact/packages/harnesses/src/enact/enact-agent-config.ts)
- Native workspace and install-root resolution: [`schema.rs`](/Users/tarunrana/workspace/enact-agent/crates/zeroclaw-config/src/schema.rs)
- Workspace export boundary: [`export.rs`](/Users/tarunrana/workspace/enact-agent/src/alias_cli/export.rs)
