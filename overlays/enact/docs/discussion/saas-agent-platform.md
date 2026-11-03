# Enact Agent as a SaaS agent platform: discussion record

Status: discussion notes, not a decision or a contract. Recorded 2026-11-03.
Upstream base examined: `1c1cd118` (ZeroClaw 0.8.7). Fork release: `0.8.7-enact.1`.

This file does not relax `overlays/enact/AGENTS.md`. Nothing here has been built,
run or qualified. Claims are tagged:

- **[verified]** read in this repository, or produced by a command run here.
- **[unverified]** general knowledge or inference. Check before relying on it.

## 1. Goal

Build a SaaS product where an agent is present on every page and can help the
user in context. Requirements stated so far:

- Gateway, daemon and ACP, so agents can call other agents.
- A desktop client, and SSH.
- Agents run inside sandboxes.
- An existing `/ws/chat` relay (TypeScript) already carries events from Codex,
  Claude and Enact agents. It works but is slow.
- Domain topics: data, dashboards, supply chain, revenue growth.

## 2. What the fork is today

**[verified]** The fork changes no Rust under `src/`, `crates/`, `Cargo.toml` or
`Cargo.lock`. `git diff 1c1cd118 HEAD -- src crates Cargo.toml Cargo.lock` is
empty. 29 files differ in total: the `overlays/enact/` directory, one CI
workflow, `nix/web.nix`, a pointer in the root `AGENTS.md`, and web branding.

**[verified]** `overlays/enact/build.py` edits a copy of the source under
`target/enact/source/` at build time. Every edit uses a checked anchor that
fails if upstream drifts:

| Edit | Effect |
| --- | --- |
| Rebranding | `ZeroClaw` becomes `Enact Agent` in locale files, Clap help, ACP title, personality template and default system-prompt identity. The prompt change is model-visible. |
| Env namespace | `ZEROCLAW_*` becomes `ENACT_AGENT_*`, only inside Rust string literals. |
| `session/tools` RPC | New method returning tool names from the live, constructed agent registry. Makes `Agent::tool_names()` public. |
| Tool status | `TurnEvent::ToolResult` gains `success: Option<bool>`: `Some` for local execution, `None` for provider-preexecuted. Added to the RPC notification. |
| Updater | Branded builds refuse to install upstream binaries. |
| Desktop | Tauri product name, titles, `Info.plist`, splash, ad-hoc signing. |
| Packaging | Headless, locked, offline, build `$HOME` remapped to `/build/home`, normalized archive ownership, `manifest.json` v1 plus `RELEASE.json`. |

## 3. The lean release leaves capability out

**[verified]** `overlays/enact/build.py` builds releases with
`--no-default-features --features agent-runtime`. Upstream's default set is
`agent-runtime`, `default-channels`, `acp-bridge`, `gateway`,
`observability-prometheus` and `schema-export`.

Not in the release:

- The gateway: no HTTP or WebSocket server, REST API, webhook ingress, A2A,
  OIDC or pairing endpoints.
- All `channel-*` features, including `channel-acp-server` and `channel-webhook`.
- Metrics, config schema export.
- Opt-in features: `tools-compat` (Jira, Notion, Google Workspace, Microsoft 365,
  Composio, and Claude Code, Codex, Gemini and OpenCode CLI delegates),
  `browser-native`, `plugins-wasm`, `sandbox-landlock`, `sandbox-bubblewrap`,
  `observability-otel`, `memory-postgres`, hardware features.

**Security note.** A headless agent that runs shell tools without a sandbox
feature is a larger gap than missing channels. If an outer container or microVM
is the boundary, record that decision explicitly.

## 4. Crate inventory

**[verified]** Produced with `cargo tree -e normal -p zeroclaw` for the lean
feature set and for `--features gateway`.

### Linked in the lean build (17 of 22)

| Crate | Lines | Role |
| --- | ---: | --- |
| `zeroclaw-runtime` | 413k | Core: agent loop, security and policy, cron, skills, observability, local RPC (the layer the fork patches), MCP client, TUI. |
| `zeroclaw-channels` | 230k | Messaging integrations. Linked, but nearly empty with no `channel-*` features. |
| `zeroclaw-providers` | 108k | LLM providers, auth, multimodal. |
| `zeroclaw-tools` | 104k | Tool implementations; optional adapters are feature-gated. |
| `zeroclaw-config` | 102k | Config schema and secrets. Pairs with `zeroclaw-macros`. |
| `zeroclaw-memory` | 31k | Memory backends, embeddings, retrieval. |
| `zeroclaw-api` | 17k | Traits and shared wire types (`TurnEvent`, A2A wire model). |
| `zeroclaw-infra` | 17k | Session backends, debouncing, stall watchdog. |
| `zeroclaw-log` | 12k | Structured event logging. |
| `zeroclaw-tool-call-parser` | 6k | Parses tool calls from many model output formats. |
| `zeroclaw-eval` | 3k | Trace replay harness. |
| `zeroclaw-tls` | 3k | mTLS construction and pinning. |
| `zeroclaw-macros` | 3k | Config derive macros. |
| `zeroclaw-commands`, `zeroclaw-sop-graph`, `zeroclaw-relay-proto`, `zeroclaw-spawn` | under 1k each | Slash commands, SOP graph types, relay wire protocol, tokio spawn wrapper. |

### Not in the lean build (5 of 22)

| Crate | Lines | Role | Relevance |
| --- | ---: | --- | --- |
| `zeroclaw-gateway` | 58k | HTTP and WebSocket gateway, REST, dashboard, webhooks, A2A, OIDC, pairing, canvas, voice | High. Adding `--features gateway` pulls in only this crate. |
| `zeroclaw-plugins` | 32k | WASM plugin host with signing | Possible third-party tool extension. |
| `zeroclaw-hardware` | 11k | Device and peripheral management | Low. |
| `zeroclaw-buildinfo`, `zeroclaw-dist` | under 1k | Build-time provenance and release-target registry | Build-time only. |

### Apps

| App | What it is | Fit |
| --- | --- | --- |
| `apps/zerocode` | Interactive TUI config manager | Operator tool, not product-facing. |
| `apps/tauri` | Desktop tray app (`zeroclaw-desktop`) | In scope: desktop client. |
| `apps/zerorelay` | Standalone relay that forwards mTLS between NAT-ed daemons and clients without decrypting it | In scope if desktop clients reach daemons behind NAT. Not needed for a server-hosted SaaS. |

## 5. Gateway surface relevant to the product

**[verified]** Routes and modules in `crates/zeroclaw-gateway`:

- `/ws/chat` WebSocket chat. `/acp` ACP over WebSocket. `/ws/canvas/{id}`,
  `/ws/nodes`, `/ws/sops/runs`.
- A2A discovery and task endpoint (`a2a.rs`), behind the gateway crate's
  default `a2a` feature.
- Auth modules: `api_oidc`, `api_pairing`, `api_webauthn`, `principal_gate`.
- `webhook_ingress`, `api_upload`, SOP authoring, OpenAPI docs.

**[unverified]** Whether the gateway's ACP route can proxy foreign agents (Codex,
Claude) or only serves ZeroClaw's own agent. Read `acp.rs` and `ws.rs` first.

**[unverified]** Whether the fork's `session/tools` and `success` fields reach
gateway consumers. The patch touches the local-RPC path in `rpc/dispatch.rs`.

## 6. Relay options: A, B and C

The existing TypeScript `/ws/chat` relay is slow because it is not Rust. Three
shapes were identified:

- **A. Own thin Rust relay.** Translates Codex and Claude events to one schema.
  Enact is one backend among several.
- **B. Enact gateway as the front door.** Codex and Claude run behind it as ACP
  agents.
- **C. Enact agent as the only agent.** Other agents are reached as tools through
  A2A.

**Decision recorded:** in some product flavours, A, B and C are all targets. The
design must not assume a single winner. Keep the event schema, the agent
transport and the orchestration layer separable so each flavour can pick a
combination.

## 7. One MCP server for every agent

Idea: expose one MCP server of domain tools (data, dashboards, supply chain,
revenue growth) so Claude, Codex and Enact all use the same tools and schema.

- **[verified]** Enact has an MCP client, and `probe_native.py` exercises it
  against a local fixture.
- **[unverified]** Limit: MCP unifies domain tools, not built-in ones. Shell,
  editing and Claude's native plan mode stay per-agent. A `plan` tool in the MCP
  server works for every agent but is not Claude's plan mode.
- Design intent: each agent runs in its own sandbox, and the MCP server is the
  only route to product data. That gives one place for tenant isolation, audit,
  per-user credentials and approvals.

## 8. Direction and order of work (owner's thoughts)

1. **Relay.** Port the TypeScript relay to Rust, using it as a reference example
   and not a line-by-line port. Future needs may differ.
2. **Gateway.** Port or adopt first. It is the foundation for the other steps.
3. **ACP, A2A or similar** for team orchestration.
4. Open question 4 from the owner is answered in section 9.

Suggested sequencing to review: define the event schema first, since both the
relay and the gateway depend on it. Treat the fork's `session/tools` and
`success` additions as part of that schema.

## 9. What A2A is

Question: is A2A the agent card, where everything is a task?

Short answer: the agent card is the discovery half, tasks are the work half, but
not everything is a task.

**[verified]** in this repository:

- Discovery: `GET /.well-known/agents-card.json` lists the agents a deployment
  publishes. `GET /a2a/{alias}/.well-known/agent-card.json` returns one agent's
  card: skills and the URL to send work to.
- Version `1.0`, transport binding `JSONRPC` (`a2a.rs`).
- Task lifecycle states (`zeroclaw_api::a2a_wire::TaskState`): submitted,
  working, completed, failed, canceled, input-required, rejected.
- Only `SendMessage` is implemented in this gateway. Other methods return
  JSON-RPC method-not-found. `SendMessage` runs a full tool-enabled agent turn
  and requires the gateway pairing bearer token. Discovery GETs are open.
- The inbound surface and the outbound client in `zeroclaw-tools` share one wire
  model, so an Enact agent can both publish itself and call other A2A agents.

**[unverified]** from general knowledge of the A2A specification, check against
the spec text before depending on it:

- A2A is an agent-to-agent protocol (originated at Google, now under the Linux
  Foundation). The pieces are: Agent Card (discovery), Message with Parts
  (conversation), Task (a stateful unit of work with a lifecycle), Artifact (task
  output).
- A send call can return either a plain message or a task. A task appears when
  work is long-running or stateful, so it is not true that everything is a task.
- Spec-level methods usually include streaming send, get task, cancel and
  subscribe. This gateway does not implement them yet.

### How A2A, ACP and MCP differ

| Protocol | Connects | Transport here |
| --- | --- | --- |
| MCP | An agent to tools and data | HTTP or stdio to an MCP server |
| ACP (Agent Client Protocol) | An editor or client to one agent session | stdio JSON-RPC (`zeroclaw acp`), plus a WebSocket route on the gateway |
| A2A | One agent to another agent, across deployments | HTTP JSON-RPC with agent cards |

For team orchestration, A2A is the cross-deployment, discoverable option. ACP is
the client-to-session option. Both can coexist, which matches the A, B and C
flavours above.

## 10. Local comparison with upstream (summary)

Install the prebuilt Enact archive from `overlays/enact/AGENTS.md`. Both binaries
default to the same config directory and gateway port (`42617`), so isolate them:

- `ENACT_AGENT_CONFIG_DIR=$HOME/.enact-test` for Enact.
- `ZEROCLAW_CONFIG_DIR=$HOME/.zeroclaw-test` for ZeroClaw.
- Use different gateway ports if both daemons run at once.

Compare `--version`, `--help`, missing capabilities and, for Enact only, the
`session/tools` RPC via `probe_native.py`.

## 11. Open questions

1. Single shared multi-tenant service, or one deployment per customer?
2. Should the on-page agent only answer, or act on user data? This sets the tool
   and approval design.
3. Use the stock dashboard widget, or build a custom UI against `/ws/chat`?
4. SSH direction: agents SSH out of the sandbox, or users SSH in? Not yet checked
   in the channels or tools crates.
5. Can gateway ACP front foreign agents (read `acp.rs`, `ws.rs`)?
6. Do `session/tools` and `success` need to reach gateway consumers?
7. Which feature profile for the SaaS build? Proposed floor: `agent-runtime`,
   `gateway`, `channel-acp-server`, `channel-webhook`, plus sandbox features on
   Linux. Optional tiers for channels, `tools-compat` and `plugins-wasm`.
8. Which feature profiles ship as separate archives, and what does each add to
   the per-target qualification burden recorded in `RELEASE.json`?

## 12. Next steps

- Read `crates/zeroclaw-gateway/src/acp.rs` and `ws.rs`; settle open question 5.
- Sketch the MCP tool surface for data, dashboards, supply chain and revenue
  growth.
- Draft a feature-profile change to `build.py`, `RELEASE.json` and the workflow
  matrix, reviewed before any build.
- Define the shared event schema before porting the relay or the gateway.
