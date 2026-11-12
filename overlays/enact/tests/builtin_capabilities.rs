//! Enact ships Always + External built-ins; OptIn adapters stay feature-gated off.

use zeroclaw_config::builtin_tools::{BuiltinToolsConfig, CORE_TOOL_NAMES};
use zeroclaw_tools::inventory::{BuiltinToolCompilation, BUILTIN_TOOLS};

#[test]
fn always_and_external_builtins_are_compiled() {
    assert_eq!(BUILTIN_TOOLS.len(), 93, "review upstream inventory drift");
    let missing: Vec<_> = BUILTIN_TOOLS
        .iter()
        .filter(|tool| {
            matches!(
                tool.compilation,
                BuiltinToolCompilation::Always | BuiltinToolCompilation::External
            )
        })
        .filter(|tool| !tool.compiled())
        .map(|tool| tool.name)
        .collect();
    assert!(
        missing.is_empty(),
        "Enact build omits Always/External built-ins: {missing:?}"
    );

    let unexpected: Vec<_> = BUILTIN_TOOLS
        .iter()
        .filter(|tool| matches!(tool.compilation, BuiltinToolCompilation::OptIn(_)))
        .filter(|tool| tool.compiled())
        .map(|tool| tool.name)
        .collect();
    assert!(
        unexpected.is_empty(),
        "Enact Always/External build unexpectedly compiles OptIn built-ins: {unexpected:?}"
    );
}

#[test]
fn full_and_minimal_selection_preserve_upstream_policy() {
    let full = BuiltinToolsConfig::default();
    let minimal = BuiltinToolsConfig { optional: vec![] };
    let named = BuiltinToolsConfig {
        optional: vec!["delegate".into(), "spawn_subagent".into()],
    };
    for tool in BUILTIN_TOOLS {
        assert!(full.is_enabled(tool.name), "full selection: {}", tool.name);
        assert_eq!(
            minimal.is_enabled(tool.name),
            CORE_TOOL_NAMES.contains(&tool.name),
            "minimal selection: {}",
            tool.name
        );
        assert_eq!(
            named.is_enabled(tool.name),
            CORE_TOOL_NAMES.contains(&tool.name)
                || ["delegate", "spawn_subagent"].contains(&tool.name),
            "named selection: {}",
            tool.name
        );
    }
}
