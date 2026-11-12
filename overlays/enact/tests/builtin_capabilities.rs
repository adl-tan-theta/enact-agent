//! Enact ships Always + External built-ins; OptIn adapters stay feature-gated off.

use zeroclaw_config::builtin_tools::{BuiltinToolsConfig, CORE_TOOL_NAMES};
use zeroclaw_runtime::tools::families::{FAMILIES, MEMBERS};
use zeroclaw_tools::inventory::{BuiltinToolCompilation, BUILTIN_TOOLS};

#[test]
fn always_and_external_builtins_are_compiled() {
    // The size of the inventory is derived, never written down: the build
    // checks the compiled inventory against the canonical registry — the
    // operations inventory plus the family table — and fails the build when
    // the two disagree. What this test owns is the inventory's shape and
    // the canonical tools the consolidation introduced.
    let mut names: Vec<&str> = BUILTIN_TOOLS.iter().map(|tool| tool.name).collect();
    let rows = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        rows,
        "the built-in inventory repeats a tool name"
    );
    assert!(rows > 0, "the built-in inventory is empty");

    // Every canonical family tool ships as an inventory row, and so does
    // every tool a family replaced.
    let missing_families: Vec<&str> = FAMILIES
        .iter()
        .map(|family| family.name)
        .filter(|name| !names.contains(name))
        .collect();
    assert!(
        missing_families.is_empty(),
        "the built-in inventory omits canonical family tools: {missing_families:?}"
    );
    let missing_members: Vec<&str> = MEMBERS
        .iter()
        .map(|member| member.tool)
        .filter(|tool| !names.contains(tool))
        .collect();
    assert!(
        missing_members.is_empty(),
        "the built-in inventory omits consolidated tools: {missing_members:?}"
    );

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
