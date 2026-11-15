//! Hook adapter: `WasmHook` implements `zeroclaw_api::hook::HookHandler`
//! backed by the `hook-plugin` component world.
//!
//! A declarative hook answers by running a shell command; a plugin hook
//! answers in process. Both are consulted by the same hook runner, at the same
//! points, so a plugin hook composes with the command hooks rather than
//! replacing them.
//!
//! A hook that fails does not get to decide. If the plugin traps, misses its
//! deadline, or answers with something the host cannot read, the runtime
//! carries on as if the hook had not been asked — a broken plugin must not be
//! able to wedge the turn it is attached to.

use crate::PluginCapability;
use crate::component::bindings::hook::HookPlugin;
use crate::component::bindings::hook::exports::zeroclaw::plugin::hook::HookResult as WitHookResult;
use crate::component::{
    PluginState, PluginStoreSpec, WarmPluginState, call_plugin, call_store, engine, load_component,
    wt, wt_instantiate,
};
use crate::host::AdmittedComponent;
use crate::instance::PluginInstanceScope;
use crate::services::PluginHostServices;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use wasmtime::Store;
use wasmtime::component::Linker;
use zeroclaw_api::hook::{HookHandler, HookResult};
use zeroclaw_api::tool::ToolOutput;
use zeroclaw_api::tool::ToolResult;

/// What a plugin hook answered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HookDecision {
    /// Carry on unchanged.
    Continue,
    /// Stop the call, with the reason the hook gave.
    Block(String),
    /// Carry on with the hook's replacement.
    Rewrite {
        /// The replacement tool arguments.
        arguments: Option<Value>,
        /// The replacement text.
        output: Option<String>,
    },
}

/// Read the host's answer out of the guest's record.
fn decision_from_wit(result: &WitHookResult) -> Result<HookDecision> {
    match result.decision.trim().to_ascii_lowercase().as_str() {
        "continue" => Ok(HookDecision::Continue),
        "block" => Ok(HookDecision::Block(result.reason.clone().unwrap_or_else(|| {
            "the plugin hook blocked the call".to_string()
        }))),
        "rewrite" => {
            let arguments = match &result.arguments {
                Some(raw) => Some(
                    serde_json::from_str(raw)
                        .map_err(|error| anyhow::anyhow!("the hook's rewritten arguments are not valid JSON: {error}"))?,
                ),
                None => None,
            };
            Ok(HookDecision::Rewrite {
                arguments,
                output: result.output.clone(),
            })
        }
        other => Err(anyhow::anyhow!(
            "the plugin hook answered with an unknown decision `{other}`"
        )),
    }
}

/// Build the hook world. The hook world imports only logging, so it needs no
/// HTTP coherence and no per-instance host grants.
fn linker() -> Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine());
    crate::component::add_wasi(&mut linker)?;
    let mut options = crate::component::bindings::hook::LinkOptions::default();
    options.plugins_wit_v0(true);
    wt(
        HookPlugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
            &mut linker,
            &options,
            |s| s,
        ),
        "failed to add hook plugin imports to linker",
    )?;
    Ok(linker)
}

/// Verify that a hook component loads.
///
/// The load check never calls a guest export: it compiles and instantiates
/// the component, which is exactly what an install-time check can afford.
pub async fn verify_hook_loads(
    component: &AdmittedComponent,
    scope: &PluginInstanceScope,
    services: &PluginHostServices,
    limits: crate::component::PluginLimits,
) -> Result<()> {
    scope.require_capability(PluginCapability::Hook)?;
    let component = load_component(component)?;
    let mut store = crate::component::new_store(PluginStoreSpec::new(
        scope.clone(),
        services.clone(),
        limits,
    ));
    let linker = linker()?;
    crate::component::ensure_http_coherent(&store, false)?;
    call_store!(store, async |store: &mut Store<PluginState>| {
        wt_instantiate(
            HookPlugin::instantiate_async(store, &component, &linker).await,
            "failed to instantiate hook plugin",
        )
        .map(|_bindings| ())
    })
}

/// A hook handler backed by a WIT component-model plugin.
pub struct WasmHook {
    scope: PluginInstanceScope,
    name: String,
    state: Arc<Mutex<WarmPluginState<HookPlugin>>>,
}

impl WasmHook {
    /// Compile and instantiate a hook plugin.
    pub async fn from_wasm(
        scope: PluginInstanceScope,
        component: &AdmittedComponent,
        services: &PluginHostServices,
        limits: crate::component::PluginLimits,
    ) -> Result<Self> {
        scope.require_capability(PluginCapability::Hook)?;
        let component = load_component(component)?;
        let mut store = crate::component::new_store(PluginStoreSpec::new(
            scope.clone(),
            services.clone(),
            limits,
        ));
        let linker = linker()?;
        crate::component::ensure_http_coherent(&store, false)?;
        let bindings = call_store!(store, async |store: &mut Store<PluginState>| {
            wt_instantiate(
                HookPlugin::instantiate_async(store, &component, &linker).await,
                "failed to instantiate hook plugin",
            )
        })?;
        let name = call_store!(store, async |store: &mut Store<PluginState>| {
            wt(
                bindings.zeroclaw_plugin_hook().call_name(store).await,
                "hook.name failed",
            )
        })?;
        Ok(Self {
            scope,
            name,
            state: Arc::new(Mutex::new(Some((store, bindings)))),
        })
    }

    /// The plugin instance this handler belongs to.
    #[must_use]
    pub fn scope(&self) -> &PluginInstanceScope {
        &self.scope
    }

    /// Ask the plugin about one event.
    ///
    /// `Ok(None)` means the plugin could not answer and the host carries on.
    async fn ask(&self, event: &str, payload: Value) -> Result<Option<HookDecision>> {
        let payload = serde_json::to_string(&payload)
            .map_err(|error| anyhow::anyhow!("the hook payload is not serializable: {error}"))?;
        let event = event.to_string();
        let answer = call_plugin!(
            self,
            "hook.handle trapped",
            async move |store: &mut Store<PluginState>, bindings: &mut HookPlugin| {
                bindings
                    .zeroclaw_plugin_hook()
                    .call_handle(store, &event, &payload)
                    .await
            }
        )?;
        match answer {
            Ok(result) => Ok(Some(decision_from_wit(&result)?)),
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "error": error,
                        })),
                    "plugin hook could not answer; the turn continues"
                );
                Ok(None)
            }
        }
    }
}

#[async_trait]
impl HookHandler for WasmHook {
    fn name(&self) -> &str {
        &self.name
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        let payload = serde_json::json!({ "tool": name, "arguments": args });
        match self.ask("before_tool_call", payload).await {
            Ok(Some(HookDecision::Block(reason))) => HookResult::Cancel(reason),
            Ok(Some(HookDecision::Rewrite {
                arguments: Some(rewritten),
                ..
            })) => HookResult::Continue((name, rewritten)),
            Ok(Some(HookDecision::Rewrite { arguments: None, .. }))
            | Ok(Some(HookDecision::Continue))
            | Ok(None) => HookResult::Continue((name, args)),
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "tool": name,
                            "error": error.to_string(),
                        })),
                    "plugin hook answered unusably; the call continues unchanged"
                );
                HookResult::Continue((name, args))
            }
        }
    }

    async fn after_tool_call(&self, tool: &str, result: &ToolResult) -> HookResult<ToolResult> {
        let payload = serde_json::json!({
            "tool": tool,
            "result": {
                "success": result.success,
                "output": result.output.as_str(),
                "error": result.error,
            }
        });
        match self.ask("after_tool_call", payload).await {
            Ok(Some(HookDecision::Block(reason))) => HookResult::Cancel(reason),
            Ok(Some(HookDecision::Rewrite {
                output: Some(output), ..
            })) => {
                let mut rewritten = result.clone();
                rewritten.output = ToolOutput::text(output);
                HookResult::Continue(rewritten)
            }
            Ok(Some(HookDecision::Rewrite { output: None, .. }))
            | Ok(Some(HookDecision::Continue))
            | Ok(None) => HookResult::Continue(result.clone()),
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "tool": tool,
                            "error": error.to_string(),
                        })),
                    "plugin hook answered unusably; the result continues unchanged"
                );
                HookResult::Continue(result.clone())
            }
        }
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        let payload = serde_json::json!({ "prompt": prompt });
        match self.ask("before_prompt_build", payload).await {
            Ok(Some(HookDecision::Block(reason))) => HookResult::Cancel(reason),
            Ok(Some(HookDecision::Rewrite {
                output: Some(output), ..
            })) => HookResult::Continue(output),
            Ok(Some(HookDecision::Rewrite { output: None, .. }))
            | Ok(Some(HookDecision::Continue))
            | Ok(None) => HookResult::Continue(prompt),
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "error": error.to_string(),
                        })),
                    "plugin hook answered unusably; the prompt continues unchanged"
                );
                HookResult::Continue(prompt)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_continue_decision_carries_on() {
        let result = WitHookResult {
            decision: "continue".to_string(),
            reason: None,
            arguments: None,
            output: None,
        };
        assert_eq!(decision_from_wit(&result).unwrap(), HookDecision::Continue);
    }

    #[test]
    fn a_block_decision_keeps_its_reason() {
        let result = WitHookResult {
            decision: "block".to_string(),
            reason: Some("not on a weekend".to_string()),
            arguments: None,
            output: None,
        };
        assert_eq!(
            decision_from_wit(&result).unwrap(),
            HookDecision::Block("not on a weekend".to_string())
        );
    }

    #[test]
    fn a_block_without_a_reason_still_blocks() {
        let result = WitHookResult {
            decision: "BLOCK".to_string(),
            reason: None,
            arguments: None,
            output: None,
        };
        assert!(matches!(
            decision_from_wit(&result).unwrap(),
            HookDecision::Block(_)
        ));
    }

    #[test]
    fn a_rewrite_decision_reads_its_arguments() {
        let result = WitHookResult {
            decision: "rewrite".to_string(),
            reason: None,
            arguments: Some("{\"path\":\"safe.txt\"}".to_string()),
            output: Some("trimmed".to_string()),
        };
        let decision = decision_from_wit(&result).unwrap();
        match decision {
            HookDecision::Rewrite { arguments, output } => {
                assert_eq!(arguments.unwrap()["path"], "safe.txt");
                assert_eq!(output.unwrap(), "trimmed");
            }
            other => panic!("expected a rewrite, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_decision_is_refused() {
        let result = WitHookResult {
            decision: "maybe".to_string(),
            reason: None,
            arguments: None,
            output: None,
        };
        assert!(decision_from_wit(&result).is_err());
    }

    #[test]
    fn arguments_that_are_not_json_are_refused() {
        let result = WitHookResult {
            decision: "rewrite".to_string(),
            reason: None,
            arguments: Some("not json".to_string()),
            output: None,
        };
        assert!(decision_from_wit(&result).is_err());
    }
}
