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
- Review the mailbox design in section 15 and settle its open questions.

## 13. Update: ACP adapters for Claude and Codex

**Owner-reported, not checked here:** both Claude and Codex have ACP adapters.

Consequence for the options in section 6: option B (Enact gateway as the front
door, other agents behind it over ACP) becomes more plausible, and a Rust relay
could drive all three agents through one protocol instead of shelling out to
`claude -p` and `codex exec`. Still **[unverified]**: whether the gateway's own
`/acp` route can front foreign agents, or only serves ZeroClaw's agent. Read
`crates/zeroclaw-gateway/src/acp.rs` and `ws.rs`, and confirm the adapters'
maturity, session resume support and tool-permission handling.

## 14. Sandboxes: shared or separate, and how data moves between them

### Sharing one sandbox across agents

Owner's idea: put all agents in one sandbox to make message passing easy.
Assessment: not bad in itself, if the scope is one tenant and one task. It is a
bad idea across tenants.

| Risk | Why | Mitigation |
| --- | --- | --- |
| Credentials leak between agents | Agent CLIs keep auth under their own home directories and in env vars. A shared `$HOME` exposes each agent's credentials to the others. | One unix user or `$HOME` per agent. Inject secrets per process. |
| Edit and git collisions | Two agents in one tree overwrite each other and contend for `.git/index.lock`. | One git worktree per agent, merged by an orchestrator. |
| One compromise spreads | A prompt-injected agent reaches whatever the others can. | One shared sandbox is one blast radius. Keep tool permissions per agent in the MCP server. |
| Attribution | Hard to tell which agent acted. | Separate users or processes, and log every MCP call with agent identity. |
| Resource contention | A runaway agent starves the rest. | cgroup limits per agent. |

Recommendation: one sandbox per team or task, never one across tenants. Each
agent gets its own user or `$HOME` and its own worktree. Pass messages through a
mailbox service, not the filesystem.

### How data moves from sandbox to sandbox

The owner did not yet know how to do this. Four patterns:

1. **Central mailbox service (recommended start).** Every sandbox has outbound
   access to one endpoint, the MCP server in section 7. Messages and artifact
   references go through it. Sandboxes never connect to each other, and the
   server enforces tenant isolation and audit.
2. **Object storage for files and large data.** The sender uploads to a bucket
   path scoped to tenant and task. The receiver gets a short-lived signed URL
   through the mailbox.
3. **Direct A2A between agents.** **[verified]** The Enact gateway can publish an
   agent card and accept `SendMessage` over HTTP behind a pairing bearer token.
   Claude and Codex would need an A2A wrapper, which is **[unverified]**. It also
   needs network routes between sandboxes. A later option for cross-deployment
   or cross-organization agents.
4. **Shared volume or one shared sandbox.** Simplest, with the risks above. If
   used, limit it to a scratch directory shared only by agents in the same task.

Start with patterns 1 and 2.

## 15. Proposal: mailbox tool list and message schema

Status: a design proposal for the one MCP server, not implemented. Nothing in this
section has been built, tested or checked against any agent CLI. How each CLI
loads MCP configuration is **[unverified]** and needs checking first.

### Principles

- **Identity comes from the credential, not the caller.** The server derives
  `tenant_id` and `agent_id` from the sandbox's scoped credential. A tool call
  can never set `from`, and can never read or write another tenant's data.
- **Message bodies are untrusted data.** A message from another agent is input,
  never an instruction from the user. The receiving agent's instructions must say
  so, because prompt injection can travel through the mailbox.
- **Artifacts by reference.** Messages carry small text or JSON. Large content
  goes to object storage and travels as an `artifact_ref`.
- **At-least-once delivery.** Reads do not delete. A message is done when the
  reader calls `ack`. Senders pass an `idempotency_key` so retries do not
  duplicate.
- **Ordering is guaranteed per thread, not globally.**
- **Everything is logged** with tenant, task, agent and tool name.

### Message envelope

```json
{
  "id": "msg_01...",
  "tenant_id": "set by server",
  "task_id": "task_01...",
  "thread_id": "thr_01...",
  "from": "agent_id set by server",
  "to": "agent_id | role:<name> | team:<task_id>",
  "kind": "message | task_request | task_update | result | error | approval_request | approval_response",
  "parts": [
    { "type": "text", "text": "..." },
    { "type": "json", "data": {} },
    { "type": "artifact_ref", "artifact_id": "art_01...", "name": "report.csv", "content_type": "text/csv" }
  ],
  "reply_to": "msg_01... or null",
  "idempotency_key": "client supplied",
  "created_at": "RFC 3339",
  "expires_at": "RFC 3339 or null"
}
```

The shape deliberately mirrors A2A's message, task and artifact model so a later
A2A bridge is a mapping, not a redesign. Task states reuse the A2A lifecycle that
is already in `zeroclaw_api::a2a_wire::TaskState` **[verified]**: submitted,
working, completed, failed, canceled, input-required, rejected.

### Tools

| Group | Tool | Purpose |
| --- | --- | --- |
| Identity | `whoami` | Return this agent's id, role, tenant, task and permissions. |
| Identity | `list_agents` | List agents and roles in the current task, with their declared skills. |
| Messages | `send_message(to, parts, kind?, thread_id?, reply_to?, idempotency_key?, expires_in?)` | Send to an agent, a role or the whole team. Returns the message id. |
| Messages | `read_inbox(thread_id?, kinds?, cursor?, limit?, wait_seconds?)` | Return unacked messages and a cursor. `wait_seconds` allows long polling, which suits one-shot CLI runs. |
| Messages | `ack(message_ids)` | Mark messages handled. |
| Messages | `get_thread(thread_id, cursor?, limit?)` | Read a conversation history. |
| Tasks | `create_task(to, title, instructions, input_refs?)` | Assign work. Starts in `submitted`. |
| Tasks | `update_task(task_id, state, note?, result_parts?)` | Move a task through its lifecycle and attach results. |
| Tasks | `get_task(task_id)` and `list_tasks(state?, assignee?)` | Inspect work. |
| Tasks | `cancel_task(task_id, reason?)` | Request cancellation. |
| Plan | `get_plan()` and `update_plan(steps)` | One shared plan format for every agent. Fills the gap where one CLI has a native plan tool and another does not. It is not the native plan mode of any CLI. |
| Artifacts | `put_artifact(name, content_type, text?)` | Store small content directly. Returns an `artifact_id`. |
| Artifacts | `request_upload(name, content_type, size)` | Return a short-lived signed upload URL for large files. |
| Artifacts | `get_artifact(artifact_id)` | Return small content, or a short-lived signed download URL. |
| Artifacts | `list_artifacts(task_id?)` | Enumerate stored outputs. |
| Human | `request_approval(action, details)` | Ask a human before a sensitive action. Returns when answered or expired. |
| Human | `notify_user(text, severity?)` | Surface a message in the product UI, not to other agents. |

Domain tools (data, dashboards, supply chain, revenue growth) live in the same
server in separate groups. Each group is gated by the agent's role, so a
mailbox-only agent cannot call revenue tools.

### Server-enforced limits

- Maximum message size and parts count; reject oversize and point to
  `put_artifact`.
- Per-agent send rate limits and inbox depth limits.
- Allowed recipients per role, for example workers may message the coordinator
  and each other, but not other tasks.
- Retention and expiry on messages and artifacts.
- Artifact content scanning, and signed URLs with short lifetimes.

### Using it with one-shot CLIs

`claude -p` and `codex exec` run once per invocation. Two workable patterns, both
**[unverified]** until tested:

- An orchestrator reads the mailbox and starts a fresh CLI run per message,
  passing the message and thread context as the prompt.
- A long-running agent loop calls `read_inbox` with `wait_seconds` and keeps its
  own session alive. Whether each CLI can resume sessions between runs must be
  confirmed from its help output.

### Open questions for this design

1. Storage: Postgres, Redis or both for messages? Which object store?
2. Is the server one process per tenant, or shared with row-level tenant checks?
3. How does a sandbox obtain its scoped credential at start, and how is it
   revoked on teardown?
4. Does the orchestrator or the agents own task assignment?
5. Do we expose the same server to the gateway and the desktop client, or only to
   sandboxed agents?
6. Build it in Rust from the start, or prototype in TypeScript against the
   existing relay and port after the schema settles?

## 16. Update: workspace as isolation boundary, durable sandboxes

Owner-stated deployment facts, recorded 2026-11-03. Not verified here.

- **One sandbox is one workspace, not one agent.** The workspace is the isolation
  boundary. Agents working in the same workspace share a sandbox.
- **Sandboxes are durable** (Azure), not ephemeral. They keep state between
  sessions.

Effects on earlier sections:

- Section 14 recommended one sandbox per team or task. A workspace-per-sandbox
  boundary is the same idea with a different unit. The per-agent mitigations still
  apply inside a shared workspace: separate unix user or `$HOME` per agent so
  credentials are not shared, one git worktree per agent, per-agent cgroup limits,
  and per-agent identity in the audit log.
- Durable sandboxes make the credential point sharper. Secrets written to disk in
  a durable sandbox persist, so inject them per process and avoid writing them to
  `$HOME`, or revoke and rotate them when a session ends.
- Durable state also means agent memory, SOP run state (`sop.persist_runs`) and
  session data can live in the sandbox. Decide what is stored in the sandbox
  versus the platform database, and how a sandbox is backed up, migrated and
  deleted.
- The mailbox in section 15 still applies for passing data between workspaces.
  Within one workspace, agents can also share files, but the mailbox keeps
  message history, attribution and approvals in one auditable place.
- Peer groups and channel configuration live in each sandbox's config. With many
  workspaces, decide how that config is provisioned and updated.

Open questions:

1. How does a durable sandbox receive and rotate its scoped credential?
2. What is the lifecycle: suspend, resume, snapshot, delete?
3. Is the agent daemon long-running inside the sandbox, or started per request?
