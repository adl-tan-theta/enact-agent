# Hooks

A hook is code the runtime runs at a lifecycle point. Hooks are how an
operator adds policy — audit, redaction, approval, gating — without patching
the agent and without shipping a plugin.

There are two ways to write one:

- **Declaratively**, as a command in the config file. A script is a hook.
- **In process**, as a `HookHandler` implementation in Rust or in a WASM
  plugin.

Both run through the same runner, with the same ordering, the same cancel
semantics, and the same panic isolation.

## Where hooks run

The runtime builds one `HookRunner` per agent from the root config. It is
constructed when the agent's config is loaded, so a hook change takes effect on
the next reload — the same rule every other config section follows.

`[hooks] enabled = false` turns the whole system off. When it is off, no
handler is constructed and no hook fires.

## The events

| Event | Fires | Can change |
|---|---|---|
| `before_tool_call` | Before a tool is dispatched. | The tool name and its arguments, or cancel the call. |
| `after_tool_call` | After a tool completed. | The result text the model reads, or cancel the result. |
| `before_llm_call` | Before the model is called. | The message vector and the model, or cancel the call. |
| `before_prompt_build` | Before the prompt is assembled. | The prompt, or cancel the turn. |
| `before_model_resolve` | Before the provider and model are resolved. | The provider and model, or cancel the turn. |
| `on_message_received` | When a channel message arrives. | The message, or drop it. |
| `on_message_sending` | Before a message is delivered. | The channel, recipient, and content, or cancel the delivery. |
| `session_start`, `session_end` | Around a session. | Nothing — observation only. |
| `after_tool_call` (observing) | After a tool completed. | Nothing — the audit surface. |

The modifying events run **sequentially, in descending priority order**, and
the first one to cancel stops the chain. The observing events run **in
parallel** and are fire-and-forget.

Two `after_tool_call` families exist on purpose:

- The **modifying** one runs before the result is logged and before it
  reaches the model. It is for changing what the model sees.
- The **observing** one (`on_after_tool_call`) runs for audit. It always sees
  the *original* result, never the rewritten one, so a redaction hook cannot
  hide a call from the audit trail.

A handler that panics is caught and logged. The remaining handlers still run,
and the operation continues with the value the panicking handler would have
produced. One broken hook never takes the agent down.

## Declarative hooks

A declarative hook maps an event to a command. The command receives the event
payload as one JSON object on stdin and answers with one JSON object on
stdout.

```toml
[[hooks.commands]]
event = "before_tool_call"
matcher = "shell"
command = "bin/audit-shell.sh"
timeout_ms = 2000

[[hooks.commands]]
event = "after_tool_call"
matcher = "file_*"
command = "bin/redact-secrets.sh"
fail_open = false
priority = 10
```

| Field | Default | Meaning |
|---|---|---|
| `event` | required | The event to observe. One of the names above. |
| `command` | required | The command to run. It receives the payload as JSON on stdin. |
| `matcher` | none | Only fire for tools whose name matches this glob (`*` and `?`). Tool events only. |
| `timeout_ms` | `5000` | How long the command may run before it is killed. |
| `fail_open` | `true` | When the command fails, continue (true) or block the operation (false). |
| `env` | none | Extra environment entries for the command. |
| `priority` | `0` | Ordering among hooks. Higher runs first. |

The command runs through `/bin/sh -c` on Unix and `cmd /C` on Windows. Its
working directory is the runtime's, and it inherits the runtime's environment
plus the entries from `env`.

### The decision

```json
{"decision": "continue"}
{"decision": "block", "reason": "not allowed here"}
{"decision": "rewrite", "arguments": {"path": "safe.txt"}}
{"decision": "rewrite", "output": "redacted"}
```

| Decision | Effect |
|---|---|
| `continue` | Nothing changes. |
| `block` | The operation is cancelled with the given reason. The reason is shown to the model and recorded. |
| `rewrite` | The arguments (before a tool call) or the output (after a tool call) are replaced. |

A command that prints a parseable decision is authoritative — the exit code is
the command's own business. A command that exits non-zero without printing a
decision, times out, or cannot be run at all is a **failure**, and `fail_open`
decides what happens next: the operation continues (`true`, the default) or
it is blocked (`false`).

The failure mode is deliberately the safe one by default. A hook that is
broken should not silently stop the agent from working, and a hook that is
*meant* to be a gate says so with `fail_open = false`.

### The payload

The payload is one JSON object on stdin. Which fields it carries depends on
the event:

| Event | Payload |
|---|---|
| `before_tool_call` | `event`, `tool`, `arguments` |
| `after_tool_call` | `event`, `tool`, `success`, `output` |
| `before_llm_call` | `event`, `model`, `message_count`, `turn_trust`, `untrusted_indices`, `untrusted_content` |
| `before_prompt_build` | `event`, `prompt` |
| `session_start`, `session_end` | `event`, `session_id`, `channel` |

### Examples

Audit every shell call:

```sh
#!/bin/sh
payload=$(cat)
printf '%s\n' "$payload" >> "$AUDIT_LOG"
echo '{"decision":"continue"}'
```

Redact a token out of anything the model is about to read:

```sh
#!/bin/sh
cat > /dev/null
echo '{"decision":"rewrite","output":"[redacted by policy]"}'
```

Block a tool that the operator has not approved:

```sh
#!/bin/sh
cat > /dev/null
echo '{"decision":"block","reason":"destructive commands are not allowed here"}'
```

## Post-call output rewriting

The `after_tool_call` modifying hook rewrites the result **before** the
runtime logs it and **before** the model reads it. That ordering is the point:
a redaction hook keeps the secret out of the log line and out of the model's
context, not just out of one surface.

```toml
[[hooks.commands]]
event = "after_tool_call"
matcher = "file_read"
command = "bin/redact-secrets.sh"
```

The rewrite replaces the text of the result. The tool's declared attachments
survive it, so a hook that swaps a caption does not silently drop a chart.

If the hook cancels the result instead, the tool call is reported as failed
and the reason becomes the output the model sees. The audit trail still
records the original call and its original result.

## Trusted and untrusted context

The `before_llm_call` path hands a handler the whole message vector, which
mixes the operator's own instructions with text that arrived from somewhere
else: a channel message, a tool result, a fetched document, another agent's
answer. A handler that wanted to sanitize the external half had no way to tell
it apart from the instructions.

The runtime now classifies every message before the hook chain runs:

| Message | Trust |
|---|---|
| Content carrying the `<<<EXTERNAL_UNTRUSTED_CONTENT` frame | Untrusted |
| A `tool` message | Untrusted |
| A `user` message, when the turn's own trust class is `Untrusted` | Untrusted |
| Everything else | Trusted |

The turn's trust class comes from the ingress envelope, so a message from an
untrusted peer is untrusted in every role it appears in, and a message from
the operator stays trusted.

A handler reads the split from the context:

```rust
async fn before_llm_call_with_context(
    &self,
    context: &LlmCallHookContext,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
) -> HookResult<()> {
    // `context.is_untrusted(index)` answers for one message;
    // `context.untrusted_indices()` lists the untrusted half.
    HookResult::Continue(())
}
```

The default implementation delegates to `before_llm_call`, so a handler that
does not care about trust needs no change.

For a declarative hook the split arrives in the payload: `turn_trust` is
`trusted` or `untrusted`, `untrusted_indices` lists the untrusted messages,
and `untrusted_content` holds their text. A `rewrite` decision from a
`before_llm_call` hook replaces the content of the **untrusted messages only**
— the operator's instructions are never touched by a declarative hook.

## Writing an in-process hook

```rust
use async_trait::async_trait;
use zeroclaw_api::hook::{HookHandler, HookResult};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::tool::ToolResult;

struct RedactSecrets;

#[async_trait]
impl HookHandler for RedactSecrets {
    fn name(&self) -> &str {
        "redact-secrets"
    }

    fn priority(&self) -> i32 {
        10
    }

    async fn after_tool_call(&self, _tool: &str, result: &ToolResult) -> HookResult<ToolResult> {
        let mut rewritten = result.clone();
        rewritten.output.map_text(|text| text.replace("hunter2", "***"));
        HookResult::Continue(rewritten)
    }
}
```

Register it with the runner:

```rust
let mut runner = HookRunner::new();
runner.register(Box::new(RedactSecrets));
```

Handlers are sorted by descending priority as they are registered, so the
order does not depend on registration order.

## A plugin hook

A declarative hook answers by running a shell command. A plugin hook answers
in process, in the same runner, at the same points — so the two compose
rather than compete.

A plugin declares the `hook` capability in its manifest and exports the
`hook-plugin` world:

```wit
@unstable(feature = plugins-wit-v0)
interface hook {
    use types.{json-string};

    record hook-result {
        decision: string,             // continue | block | rewrite
        reason: option<string>,
        arguments: option<json-string>,
        output: option<string>,
    }

    name: func() -> string;
    handle: func(event: string, payload: json-string) -> result<hook-result, string>;
}
```

`event` names the event — `before_tool_call`, `after_tool_call`,
`before_prompt_build` — and `payload` carries that event's JSON description.
The decision is one of:

- `continue` — carry on unchanged;
- `block` — stop the call, with the reason in `reason`;
- `rewrite` — carry on with the replacement in `arguments` (a tool call) or
  `output` (a tool result).

A hook that fails does not get to decide. A plugin that traps, misses its
deadline, or answers with something the host cannot read leaves the turn as
if the hook had not been asked: a broken plugin must not be able to wedge the
turn it is attached to.
