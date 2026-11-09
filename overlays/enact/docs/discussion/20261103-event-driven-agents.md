# Event-driven agents: webhook, MQTT, AMQP and SOPs

Status: discussion notes, not a decision or a contract. Recorded 2026-11-03.
Companion to [saas-agent-platform.md](saas-agent-platform.md). Upstream base
examined: `1c1cd118` (ZeroClaw 0.8.7).

This file does not relax `overlays/enact/AGENTS.md`. Nothing here has been built,
run or qualified. Claims are tagged:

- **[verified]** read in this repository's documentation or code.
- **[unverified]** inference, or not checked beyond the docs.

Everything below comes from reading `docs/book/src/channels/` and
`docs/book/src/sop/`. No channel or SOP was executed.

## 1. Why this matters for the product

The on-page agent (`/ws/chat` through the gateway) is interactive: a user is
present. Event-driven agents are proactive: a notification from the product, such
as a stock-out alert, a revenue drop or a pipeline failure, wakes an agent with no
user on the page. A rich platform needs both.

## 2. Event-source channels

**[verified]** from `docs/book/src/channels/`:

| Channel | Behaviour | Build feature |
| --- | --- | --- |
| `webhook` | Embedded HTTP server on a configured port. Accepts `POST {listen_path}` with JSON `{sender, content, thread_id?}`, hands it to the agent, and can POST replies to a URL. Requires an HMAC-SHA256 `secret` (`X-Webhook-Signature: sha256=<hex>`) and refuses to start without one. Returns `400` on bad input, `401` on bad signature, `503` on backpressure. | `channel-webhook` |
| `mqtt` | Subscribes to broker topics and feeds each message to the agent loop or the SOP engine. MQTT 3.1.1, TLS verified against the system trust store. | `channel-mqtt` |
| `amqp` | Same role for AMQP message queues. | `channel-amqp` |
| `filesystem` | Reacts to file events. | `channel-filesystem` |

Note: the `webhook` channel is not the gateway's own `POST /webhook` endpoint,
which serves paired clients and lives under `[gateway]`. The gateway also has
`webhook_ingress.rs`. **[unverified]** how that module relates to the channel.

## 3. SOPs: events become workflows

**[verified]** from `docs/book/src/sop/how-it-works.md` and `fan-in/`:

- An SOP is `<shared>/sops/<name>/SOP.toml` (trigger and steps) plus an optional
  `SOP.md` body. `zeroclaw sop` manages definitions offline: `list`, `validate`,
  `show`, `graph`, `rename`, `delete`. There is no `run` subcommand.
- Runs start from a live event fan-in (authenticated webhook, MQTT, filesystem,
  AMQP), from the daemon's maintenance tick for `cron` triggers, or from the
  in-agent `sop_execute` tool.
- Calendar and peripheral triggers are defined and matched but **not yet wired**
  to a live event source.
- Run progression uses tools: `sop_status`, `sop_approve`, `sop_advance`. A step
  can be `ExecuteStep` (agent loop) or `WaitApproval` (a human approves).
- Run state is process-local by default. With `sop.persist_runs = true`, the
  default SQLite backend stores runs under `<data_dir>/sop/runs.db` and restores
  active runs after a restart. If initialization fails, it logs a warning and
  falls back to memory.
- Audit records are written to the configured memory backend under category
  `sop`.
- A trigger can ask a model to choose among several SOPs, but only among ones the
  author wrote. The model cannot add triggers.

## 4. How it fits the platform

```
Product event --> webhook / MQTT / AMQP --> SOP engine --> agent run (sandbox)
                                                |
                                                +-- WaitApproval --> user sees it in the page widget
                                                +-- result --> mailbox / notify_user
```

- **On-page agent:** interactive, through the gateway `/ws/chat`.
- **Background agent:** proactive, through event channels and SOPs.
- Both can share the mailbox MCP server and sandboxes described in
  `saas-agent-platform.md` sections 14 and 15. `WaitApproval` maps naturally to
  the `request_approval` tool there.

## 5. Feature profile impact

None of these are in the lean release, which builds with
`--no-default-features --features agent-runtime`. For this use case the SaaS
feature profile (see `saas-agent-platform.md` section 11, question 7) would add:

- `channel-webhook`, and `channel-mqtt` and/or `channel-amqp` depending on the
  product's event transport.
- `gateway`, which the approval and chat path needs.
- SOP persistence enabled in configuration (`sop.persist_runs = true`).

Each added feature widens what must be qualified per target in `RELEASE.json`
and `probe_native.py`.

## 6. Open issues and risks

1. **Tenant routing is not built in. [unverified]** Each channel binds one port
   and uses one secret per configuration. Which tenant's agent a message wakes
   must come from the product design: one deployment per tenant, or a tenant
   field in the payload that the platform validates. No built-in tenant routing
   was found; this needs a code check.
2. **Payloads are untrusted input.** A webhook, MQTT or AMQP message must be
   treated as data, never as instructions from the user. SOP triggers and
   `condition` expressions should validate payload shape before an agent sees it.
3. **Run durability.** State is process-local unless persistence is enabled. Check
   restart behaviour before relying on long-running approvals.
4. **Calendar triggers are unwired.** Time-based product events need `cron` or an
   external scheduler posting to the webhook.
5. **HMAC secret handling.** The webhook channel requires a secret. Decide how the
   platform issues, stores and rotates it per tenant.
6. **Backpressure.** The webhook channel returns `503` when its queue is full, so
   the producer must retry with backoff and an idempotency key.

## 7. Questions for the product

1. Does the product already emit events over a broker (MQTT or AMQP), or only over
   HTTP? This decides which channel features to include.
2. One deployment per tenant, or shared with payload-level tenant routing?
3. Which events should start background agents first (alerts, scheduled reports,
   data-quality checks)?
4. Where do approvals appear to the user, and how long may a run wait?
5. Should SOPs be authored by the platform, by customers, or both? Customer-authored
   SOPs raise a separate security review.

## 8. Next steps

- Read `crates/zeroclaw-gateway/src/webhook_ingress.rs` and the webhook channel
  to see how they relate and where tenant identity could attach.
- Read `docs/book/src/sop/fan-in/overview.md` and `syntax.md` for trigger
  conditions and authentication details.
- Draft a small example SOP for one product event to test the end-to-end flow,
  after the feature profile is agreed and a build exists.
