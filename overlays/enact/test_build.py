"""Boundary checks for the staged distribution's environment namespace."""
import importlib.util
import inspect
from pathlib import Path
import tempfile
import unittest
import shutil
import hashlib
import json
import re
import tarfile
from unittest.mock import patch
import xml.etree.ElementTree as ET

SPEC = importlib.util.spec_from_file_location("enact_build", Path(__file__).with_name("build.py"))
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)


class EnvironmentProjectionTests(unittest.TestCase):
    def test_literals_cover_bootstrap_schema_macros_and_buildinfo(self):
        source = '''use zeroclaw_config::Config;
const ZEROCLAW_INTERNAL: &str = "ZEROCLAW_CONFIG_DIR";
const PREFIX: &str = "ZEROCLAW_";
const VERSION_ENV: &str = "ZEROCLAW_VERSION";
let version = env!("ZEROCLAW_VERSION");
let name = concat!("ZEROCLAW_", "SOCKET");
let example = format!("ZEROCLAW_{}", path);
let raw = r###"escaped \" ZEROCLAW_SOCKET"###;
let bytes = br#"ZEROCLAW_SOCKET"#;
let ordinary = b"ZEROCLAW_SOCKET";
let c = c"ZEROCLAW_SOCKET";
let escaped = "quote: \\\" ZEROCLAW_SOCKET";
'''
        expected = source.replace('"ZEROCLAW_', '"ENACT_AGENT_').replace(' ZEROCLAW_SOCKET', ' ENACT_AGENT_SOCKET')
        self.assertEqual(build.project_environment_literals(source), expected)
        self.assertEqual(build.project_environment_literals(expected), expected)

    def test_docker_build_args_keep_their_upstream_namespace(self):
        source = '''const ARG: &str = "ZEROCLAW_BASE_RUST_SLIM";
const MIXED: &str = "ZEROCLAW_BASE_NODE ZEROCLAW_SOCKET";
const CARGO_FLAGS: &str = "ZEROCLAW_CARGO_FLAGS";
'''
        expected = '''const ARG: &str = "ZEROCLAW_BASE_RUST_SLIM";
const MIXED: &str = "ZEROCLAW_BASE_NODE ENACT_AGENT_SOCKET";
const CARGO_FLAGS: &str = "ZEROCLAW_CARGO_FLAGS";
'''
        self.assertEqual(build.project_environment_literals(source), expected)

    def test_installer_accepts_enact_feature_override_and_upstream_alias(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            shutil.copy2(build.ROOT / "install.sh", root / "install.sh")
            build.project_enact_installer_environment(root)
            installer = (root / "install.sh").read_text()
        self.assertIn('"${ENACT_AGENT_CARGO_FEATURES:-}"', installer)
        self.assertIn('$ENACT_AGENT_CARGO_FEATURES', installer)
        self.assertIn('ENACT_AGENT_CARGO_FEATURES  Extra features', installer)
        self.assertIn('feature environment override', installer)
        self.assertIn('"${ZEROCLAW_CARGO_FEATURES:-}"', installer)
        self.assertIn('$ZEROCLAW_CARGO_FEATURES', installer)


    def test_identifiers_comments_characters_and_notices_are_unchanged(self):
        source = '''// Copyright ZEROCLAW_HOLDER. "ZEROCLAW_SOCKET"
/* Outer "ZEROCLAW_CONFIG_DIR" /* nested */ ZEROCLAW_NAME */
const ZEROCLAW_INTERNAL: char = '"';
const MORE: char = '\\'';
fn borrowed<'a>(v: &'a str) -> &'a str { v }
use zeroclaw_runtime::rpc;
'''
        self.assertEqual(build.project_environment_literals(source), source)

    def test_project_only_rust_files_and_preserve_license_bytes(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            original = b'Copyright upstream\r\nZEROCLAW_NOTICE\n'
            (root / "NOTICE").write_bytes(original)
            (root / "config.toml").write_text('key = "ZEROCLAW_SOCKET"\n')
            (root / "main.rs").write_text('env!("ZEROCLAW_VERSION");\n')
            build.project_environment(root)
            self.assertEqual((root / "NOTICE").read_bytes(), original)
            self.assertEqual((root / "config.toml").read_text(), 'key = "ZEROCLAW_SOCKET"\n')
            self.assertEqual((root / "main.rs").read_text(), 'env!("ENACT_AGENT_VERSION");\n')

    def test_all_existing_rust_sources_project_idempotently(self):
        files = build.subprocess.check_output(
            ["git", "ls-files", "-z", "--", "*.rs"], cwd=build.ROOT
        ).decode().split("\0")
        count = 0
        for name in filter(None, files):
            source = (build.ROOT / name).read_text()
            projected = build.project_environment_literals(source)
            self.assertEqual(build.project_environment_literals(projected), projected, name)
            count += projected != source
        self.assertGreater(count, 80)


class ExcludedMemoryProjectionTests(unittest.TestCase):
    def test_excluded_memory_uses_none_backend_and_skips_native_rebind(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            relatives = (
                "crates/zeroclaw-runtime/src/agent/agent.rs",
                "crates/zeroclaw-runtime/src/tools/delegate.rs",
            )
            for relative in relatives:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            delegate = source / relatives[1]
            build.replace_required(
                delegate,
                "        let target_workspace = config.agent_workspace_dir(agent_name);\n"
                "        let skills = if construction == crate::composition::RegistryConstruction::NativeCompat {\n"
                "            crate::skills::load_skills_for_agent_from_config(config, agent_name)\n"
                "        } else {\n"
                "            Vec::new()\n"
                "        };\n\n"
                "        let assembled = crate::tools::scoped::ScopedToolRegistry::assemble_prepared(\n",
                "        let target_workspace = config.agent_workspace_dir(agent_name);\n"
                "        let skills = if construction == crate::composition::RegistryConstruction::NativeCompat {\n"
                "            crate::skills::load_skills_for_agent_from_config(config, agent_name)\n"
                "        } else {\n"
                "            Vec::new()\n"
                "        };\n"
                "        let caller_scope = current_delegate_caller_tool_selector();\n\n"
                "        let assembled = crate::tools::scoped::ScopedToolRegistry::assemble_prepared(\n",
            )
            build.replace_required(
                delegate,
                "                runtime,\n"
                "                caller_allowed: None,\n"
                "                connect_mcp: true,\n",
                "                runtime,\n"
                "                caller_allowed: caller_scope.as_deref(),\n"
                "                connect_mcp: true,\n",
            )
            build.replace_required(
                delegate,
                "                let bounded_default_config = Config::default();\n"
                "                let bounded_security = Arc::new(SecurityPolicy::default());\n"
                "                let assembled_bounded =\n",
                "                let bounded_default_config = Config::default();\n"
                "                let bounded_security = Arc::new(SecurityPolicy::default());\n"
                "                let caller_scope = current_delegate_caller_tool_selector();\n"
                "                let assembled_bounded =\n",
            )
            build.project_enact_excluded_memory_isolation(source)
            agent, delegate = ((source / relative).read_text() for relative in relatives)
        self.assertIn("if exclude_memory {", agent)
        self.assertIn('Arc::new(zeroclaw_memory::NoneMemory::new("none"))', agent)
        self.assertIn("if self.inject_memory {", agent)
        self.assertIn(
            "self.tools\n                .rebind_memory_tools",
            agent,
        )
        self.assertIn("let memory_free = current_delegate_caller_memory_free();", delegate)
        self.assertIn('Arc::new(zeroclaw_memory::NoneMemory::new("none"))', delegate)
        self.assertIn("exclude_memory: memory_free", delegate)


class RelayDialProjectionTests(unittest.TestCase):
    def test_dial_cancellation_uses_a_deterministic_pending_connector(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / "crates/zeroclaw-runtime/src/relay.rs"
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(build.ROOT / "crates/zeroclaw-runtime/src/relay.rs", target)
            build.project_enact_relay_dial_test_seam(source)
            projected = target.read_text()
        self.assertIn("async fn bridge_conn_with_dialer<F, Fut>(", projected)
        self.assertIn("connect(socket, remote)", projected)
        self.assertIn("std::future::pending::<std::io::Result<tokio::net::TcpStream>>()", projected)
        self.assertNotIn("fn stalled_listener", projected)


class ContainerEnvironmentProjectionTests(unittest.TestCase):
    def test_compose_runtime_overrides_follow_enact_environment_namespace(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            shutil.copy2(build.ROOT / "docker-compose.yml", source / "docker-compose.yml")
            build.project_enact_container_environment(source)
            compose = (source / "docker-compose.yml").read_text()
        self.assertIn("ENACT_AGENT_gateway__host=0.0.0.0", compose)
        self.assertIn("ENACT_AGENT_gateway__allow_public_bind=true", compose)
        self.assertIn("ENACT_AGENT_gateway__port=${ENACT_AGENT_GATEWAY_PORT:-42617}", compose)
        self.assertIn("${HOST_PORT:-127.0.0.1:42617}:${ENACT_AGENT_GATEWAY_PORT:-42617}", compose)
        self.assertNotIn("ZEROCLAW_gateway__", compose)
        self.assertNotIn("${ZEROCLAW_GATEWAY_PORT", compose)


class ComponentFixtureProjectionTests(unittest.TestCase):
    def test_integration_fixtures_follow_enact_command_and_workspace_contracts(self):
        paths = (
            "tests/component/agents_export_cli.rs",
            "tests/component/config_patch_cli.rs",
            "tests/component/acp_session_cwd_stdio.rs",
            "tests/component/cron_help_examples.rs",
        )
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in paths:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_enact_component_fixtures(source, build.manifest())
            export, ownership, acp, cron = ((source / relative).read_text() for relative in paths)

        self.assertIn('install.path().join("workspace/agents/export_fixture")', export)
        self.assertNotIn('install.path().join("agents/export_fixture/workspace")', export)
        self.assertIn("Cannot run `enact-agent agent` while another Enact Agent process owns", ownership)
        self.assertIn("`enact-agent agent --agent missing` is not configured", ownership)
        self.assertIn('ws.ends_with("workspace/agents/test-agent")', acp)
        self.assertEqual(cron.count("enact-agent cron"), 12)
        self.assertNotIn("zeroclaw cron", cron)

    def test_runtime_cli_alias_and_cron_example_use_enact_fluent_keys(self):
        relatives = ("src/main.rs", "src/cron/mod.rs")
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in relatives:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_enact_cli_command_strings(source, build.manifest())
            main, cron = ((source / relative).read_text() for relative in relatives)

        self.assertEqual(main.count('"cli-enact-agent-alias-not-configured"'), 2)
        self.assertNotIn('"`zeroclaw agent --agent {agent_alias}` is not configured', main)
        self.assertIn('get_required_cli_string("cli-enact-cron-empty-usage")', cron)
        self.assertNotIn('println!("  zeroclaw cron add', cron)


class WorkspacePathProjectionTests(unittest.TestCase):
    def test_workspace_export_guards_follow_the_enact_layout(self):
        relatives = (
            "crates/zeroclaw-config/src/schema.rs",
            "crates/zeroclaw-config/src/platform/native.rs",
            "crates/zeroclaw-config/src/agent_bundle.rs",
            "src/alias_cli/export.rs",
        )
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in relatives:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_workspace_owned_agent_paths(source)
            exporter = (source / relatives[-1]).read_text()

        self.assertIn('install.path().join("workspace/agents")', exporter)
        self.assertIn('install.path().join("workspace")', exporter)
        self.assertIn('vec!["workspace".to_string()]', exporter)
        self.assertIn('agents/researcher/host.md', exporter)
        self.assertNotIn('researcher/workspace/host.md', exporter)


class SessionToolsProjectionTests(unittest.TestCase):
    def test_actual_upstream_rpc_guards_registry_and_inventory_getter(self):
        paths = ["crates/zeroclaw-runtime/src/rpc/dispatch.rs",
                 "crates/zeroclaw-runtime/src/agent/agent.rs",
                 "crates/zeroclaw-rpc-proto/src/method.rs",
                 "crates/zeroclaw-rpc-proto/src/types.rs",
                 "crates/zeroclaw-rpc-proto/src/schema.rs"]
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in paths:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_session_tools(source)
            dispatch = (source / paths[0]).read_text()
            method = (source / paths[2]).read_text()
            proto_types = (source / paths[3]).read_text()
            catalog = (source / paths[4]).read_text()
            # The wire contract is upstream's: the variant, its wire name, its
            # declared contract and its schema all live in the proto crate.
            self.assertIn('    SessionTools,\n', method)
            self.assertIn('(Method::SessionTools, "session/tools")', method)
            self.assertIn('M::SessionTools => (Typed("SessionIdParams"), Typed("SessionToolsResult"))',
                          method)
            self.assertIn('pub struct SessionToolsResult {', proto_types)
            self.assertIn('SessionToolsResult,', catalog)
            self.assertIn('| M::SessionTools\n', dispatch)
            self.assertIn('| Method::SessionTools\n', dispatch)
            handler = dispatch.split('    async fn handle_session_tools(', 1)[1].split(
                '    async fn handle_session_state(', 1)[0]
            for guard in ('authorize_session_owner', 'capture_session_access',
                          'recheck_authority_after_admission', 'scoped_principal_id_from',
                          'revalidate_admitted_session', 'ensure_session_incarnation'):
                self.assertIn(guard, handler)
            self.assertIn('agent.tool_names()', handler)
            self.assertNotIn('self.ctx.config', handler)
            self.assertNotIn('provider', handler)
            self.assertNotIn('#[cfg(test)]\n    pub fn tool_names', (source / paths[1]).read_text())
            with self.assertRaisesRegex(RuntimeError, 'Upstream overlay drift'):
                build.project_session_tools(source)


class ToolResultStatusProjectionTests(unittest.TestCase):
    def test_every_known_producer_preserves_authority_and_preexecution_is_unknown(self):
        files = ["crates/zeroclaw-api/src/agent.rs",
                 "crates/zeroclaw-runtime/src/agent/tool_execution.rs",
                 "crates/zeroclaw-runtime/src/agent/turn/events.rs",
                 "crates/zeroclaw-runtime/src/agent/turn/stream_consume.rs",
                 "crates/zeroclaw-runtime/src/rpc/dispatch.rs",
                 "crates/zeroclaw-rpc-proto/src/types.rs",
                 "crates/zeroclaw-runtime/src/rpc/turn.rs",
                 "crates/zeroclaw-channels/src/orchestrator/acp_server.rs"]
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in files:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_tool_result_status(source)
            self.assertIn('success: Some(out.success)', (source / files[1]).read_text())
            self.assertIn('success: Some(outcome.success)', (source / files[2]).read_text())
            preexecuted = (source / files[3]).read_text().split(
                'StreamEvent::PreExecutedToolResult { name, output } => {', 1)[1].split(
                'StreamEvent::TextDelta(', 1)[0]
            self.assertIn('success: None', preexecuted)
            self.assertNotIn('success: Some(', preexecuted)
            self.assertIn('success: *success', (source / files[4]).read_text())
            self.assertIn('success: Option<bool>', (source / files[5]).read_text())
            self.assertIn('native_status_preserves_true_false_and_unknown_provider_preexecution',
                          (source / files[4]).read_text())


class BuiltinCapabilitiesTests(unittest.TestCase):
    paths = ("Cargo.toml", "crates/zeroclaw-tools/src/inventory.rs",
             "crates/zeroclaw-runtime/src/tools/mod.rs",
             "overlays/enact/tests/builtin_capabilities.rs")

    def copy_inputs(self, source):
        for relative in self.paths:
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(build.ROOT / relative, target)

    def test_actual_upstream_capabilities_project_without_changing_originals(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.copy_inputs(source)
            before = {path: (source / path).read_bytes() for path in self.paths}
            build.project_enact_agent_capabilities(source)
            for path, content in before.items():
                if path != "Cargo.toml":
                    self.assertEqual((source / path).read_bytes(), content)
            self.assertEqual(
                (source / "Cargo.toml").read_text(),
                before["Cargo.toml"].decode().rstrip()
                + '\n\n[[test]]\n'
                'name = "enact_builtin_capabilities"\n'
                'path = "tests/enact_builtin_capabilities.rs"\n'
                'required-features = ["agent-runtime", "tools-external"]\n',
            )
            self.assertEqual((source / "tests/enact_builtin_capabilities.rs").read_bytes(),
                             before[self.paths[-1]])
            with self.assertRaisesRegex(RuntimeError, "already exists"):
                build.project_enact_agent_capabilities(source)

    def test_missing_bundle_members_and_inventory_drift_fail_closed(self):
        mutations = (
            ("Cargo.toml",
             'tools-external = ["zeroclaw-runtime/tools-external", "zeroclaw-tools/tools-external"]\n',
             '', "tools-external"),
            ("crates/zeroclaw-tools/src/inventory.rs", 'name: "delegate",',
             'name: "renamed_delegate",', "delegate inventory entry"),
            ("crates/zeroclaw-tools/src/inventory.rs", 'name: "calculator",',
             'name: "shell",', "repeats a tool name"),
            ("crates/zeroclaw-runtime/src/tools/mod.rs", 'register_tool!("spawn_subagent"',
             'register_tool!("renamed_spawn_subagent"', "expected one"),
        )
        for relative, old, new, error in mutations:
            with self.subTest(path=relative, anchor=old), tempfile.TemporaryDirectory() as name:
                source = Path(name)
                self.copy_inputs(source)
                target = source / relative
                text = target.read_text()
                self.assertEqual(text.count(old), 1, "mutation must reach its real upstream anchor")
                target.write_text(text.replace(old, new))
                with self.assertRaisesRegex(RuntimeError, error):
                    build.project_enact_agent_capabilities(source)

    def test_full_build_selects_always_and_external_tools(self):
        brand = build.manifest()
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            source = root / "source"
            (source / "web/dist").mkdir(parents=True)
            (source / "web/dist/index.html").write_text("build prerequisite fixture")
            with patch.object(build, "ROOT", root), patch.object(build, "SOURCE", source), \
                    patch.object(build, "OUT", root), patch.object(build, "run") as run, \
                    patch.object(build.shutil, "copy2"), patch.object(build.shutil, "copytree"):
                # Atomic replacement still needs the staged file to exist.
                (root / "bin").mkdir()
                (root / "bin/.zeroclaw-new").touch()
                build.build(brand, debug=True)
            args = next(call.args for call in run.call_args_list if len(call.args) > 1 and call.args[1] == "build")
            features = args[args.index("--features") + 1].split(",")
            self.assertEqual(features, ["agent-runtime", "tools-external", "embedded-web"])
            self.assertEqual(build.NATIVE_FEATURES, ("agent-runtime", "tools-external"))
            capability = next(call.args for call in run.call_args_list if len(call.args) > 1 and call.args[1] == "test")
            self.assertIn("--test", capability)
            self.assertIn("enact_builtin_capabilities", capability)
            self.assertEqual(capability[capability.index("--features") + 1].split(","), features)

    def test_opt_in_features_in_native_selection_fail_closed(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.copy_inputs(source)
            with patch.object(build, "NATIVE_FEATURES", ("agent-runtime", "tools-compat")):
                with self.assertRaisesRegex(RuntimeError, "must omit OptIn tool features"):
                    build.project_enact_agent_capabilities(source)


class CanonicalToolInventoryTests(unittest.TestCase):
    """The advertised tool set is derived, and the guard judges the shipped tree."""

    def write_inventory(self, source, names):
        target = source / "crates/zeroclaw-tools/src/inventory.rs"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(
            "pub const BUILTIN_TOOLS: &[BuiltinToolSpec] = &[\n"
            + "".join(
                "    BuiltinToolSpec {\n"
                f'        name: "{name}",\n'
                "        tier: ToolTier::Core,\n"
                "        compilation: BuiltinToolCompilation::Always,\n"
                "    },\n"
                for name in names
            )
            + "];\n"
        )
        return target

    def operations_tools(self):
        rows = (build.ROOT / "overlays/enact/zeroclaw-tools-operations.csv").read_text().splitlines()[1:]
        return {line.split(",", 1)[0].lstrip("#") for line in rows if line.strip()}

    def family_tools(self):
        text = (build.ROOT / "overlays/enact/native/tools/families.rs").read_text()
        return set(re.findall(r'^        name: "([^"]+)",$', text, flags=re.MULTILINE))

    def test_canonical_set_is_the_operations_inventory_plus_the_family_tools(self):
        operations = self.operations_tools()
        families = self.family_tools()
        self.assertTrue(operations)
        self.assertTrue(families)
        self.assertEqual(build.enact_canonical_tool_names(), operations | families)
        # The family tools are canonical names in their own right: the
        # consolidation adds inventory rows the operations inventory does
        # not list, which is why the advertised count is derived.
        self.assertFalse(families & operations)

    def test_guard_accepts_the_canonical_tool_set(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.write_inventory(source, sorted(build.enact_canonical_tool_names()))
            build.check_enact_tool_inventory(source)

    def test_guard_fails_closed_when_a_canonical_tool_is_missing(self):
        expected = build.enact_canonical_tool_names()
        missing = sorted(expected)[0]
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.write_inventory(source, sorted(expected - {missing}))
            with self.assertRaisesRegex(RuntimeError, "missing from the inventory"):
                build.check_enact_tool_inventory(source)

    def test_guard_fails_closed_when_an_unlisted_tool_is_registered(self):
        expected = build.enact_canonical_tool_names()
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.write_inventory(source, sorted(expected | {"not_a_canonical_tool"}))
            with self.assertRaisesRegex(RuntimeError, "registered but not listed"):
                build.check_enact_tool_inventory(source)

    def test_guard_fails_closed_when_a_tool_is_repeated(self):
        expected = sorted(build.enact_canonical_tool_names())
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.write_inventory(source, expected + [expected[0]])
            with self.assertRaisesRegex(RuntimeError, "repeats a tool name"):
                build.check_enact_tool_inventory(source)

    def test_upstream_inventory_alone_does_not_satisfy_the_canonical_set(self):
        upstream = build.ROOT / "crates/zeroclaw-tools/src/inventory.rs"
        names = set(re.findall(r'^\s+name: "([^"]+)",$', upstream.read_text(), flags=re.MULTILINE))
        expected = build.enact_canonical_tool_names()
        self.assertTrue(names < expected, "upstream must be a strict subset of the canonical set")
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / "crates/zeroclaw-tools/src/inventory.rs"
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(upstream, target)
            with self.assertRaisesRegex(RuntimeError, "missing from the inventory"):
                build.check_enact_tool_inventory(source)

    def test_prepare_checks_the_inventory_it_ships(self):
        self.assertIn("check_enact_tool_inventory(SOURCE)", inspect.getsource(build.prepare))


class ConsolidatedFamilyTests(unittest.TestCase):
    """The consolidated families stay in step with the tools they replace.

    A family is an aggregate of its members, so two things follow. The
    inventory row it gets must carry the compilation group of its members:
    a row that claims `Always` for a family whose members sit behind
    `tools-external` promises a tool a default build cannot deliver. And
    the scope filter must drop a family when the filter drops all of its
    members: upstream removes the persistent memory tools by name, and a
    family's own name is not one of theirs.
    """

    #: The tools upstream removes from a run that asks for no memory.
    MEMORY_TOOL_NAMES = (
        "memory_store",
        "memory_recall",
        "memory_forget",
        "memory_export",
        "memory_purge",
    )

    def family_table(self):
        return build._family_table(build.ROOT / "overlays/enact/native/tools/families.rs")

    def upstream_compilations(self, inventory_text):
        """The compilation group of every inventoried tool, read independently."""
        groups = {}
        for name, tier, compilation in re.findall(
            r'name:\s*"([^"]+)",\s*\n\s*tier:\s*ToolTier::(\w+),\s*\n\s*'
            r"compilation:\s*BuiltinToolCompilation::(\w+)",
            inventory_text,
        ):
            groups[name] = compilation
        return groups

    def copy_inputs(self, source, relatives=(
        "crates/zeroclaw-tools/src/inventory.rs",
        "crates/zeroclaw-config/src/builtin_tools.rs",
    )):
        for relative in relatives:
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(build.ROOT / relative, target)

    def test_the_shipped_family_rows_carry_the_compilation_of_their_members(self):
        families, _ = self.family_table()
        upstream = self.upstream_compilations(
            (build.ROOT / "crates/zeroclaw-tools/src/inventory.rs").read_text()
        )
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.copy_inputs(source)
            build.project_enact_family_inventory(source)
            inventory = (source / "crates/zeroclaw-tools/src/inventory.rs").read_text()
        block = inventory.split(
            "// Tier 2: host-coupled. The consolidated families.\n", 1
        )[1].split("// Tier 3: optional. Utilities.\n", 1)[0]
        rows = re.findall(
            r'name:\s*"([^"]+)",\s*\n\s*tier:\s*ToolTier::Host,\s*\n\s*'
            r"compilation:\s*BuiltinToolCompilation::(\w+)",
            block,
        )
        shipped = dict(rows)
        self.assertEqual(set(shipped), set(families))
        for family, members in families.items():
            groups = {upstream[member] for member in members}
            self.assertEqual(
                groups,
                {"Always"} if shipped[family] == "Always" else {"External"},
                f"the {family} family mixes compilation groups {sorted(groups)}",
            )

    def test_a_family_compilation_group_that_mixes_its_members_fails_closed(self):
        families, _ = self.family_table()
        mixed = dict(families)
        mixed["memory"] = ("memory_store", "email_search")
        with self.assertRaisesRegex(RuntimeError, "span the compilation groups"):
            build.enact_family_compilations(
                (build.ROOT / "crates/zeroclaw-tools/src/inventory.rs").read_text(), mixed
            )

    def test_a_family_member_that_is_not_inventoried_fails_closed(self):
        families, _ = self.family_table()
        unknown = dict(families)
        unknown["memory"] = ("memory_store", "memory_not_inventoried")
        with self.assertRaisesRegex(RuntimeError, "are not inventoried"):
            build.enact_family_compilations(
                (build.ROOT / "crates/zeroclaw-tools/src/inventory.rs").read_text(), unknown
            )

    def test_no_family_mixes_memory_and_other_members(self):
        """The scope filter drops a family when it drops every member.

        The filter drops the memory tools by name. A family whose members
        were half memory and half something else would survive on the
        strength of a member that was never registered, so the families
        have to be homogeneous in their memory members.
        """
        families, _ = self.family_table()
        for family, members in families.items():
            memory = [m for m in members if m in self.MEMORY_TOOL_NAMES]
            other = [m for m in members if m not in self.MEMORY_TOOL_NAMES]
            self.assertFalse(
                memory and other,
                f"the {family} family mixes memory members {memory} with {other}",
            )

    def test_no_family_carries_a_tool_the_filter_drops_by_itself(self):
        """`deliver_file` is dropped in an ACP delivery turn, by name."""
        families, _ = self.family_table()
        for family, members in families.items():
            self.assertNotIn("deliver_file", members, f"the {family} family carries deliver_file")

    def test_no_family_is_also_a_member(self):
        """The family lookup recurses into members; it has to terminate."""
        families, _ = self.family_table()
        members = {member for names in families.values() for member in names}
        self.assertEqual(families.keys() & members, set())

    def test_an_external_family_is_named_in_the_canonical_adapter_list(self):
        """Upstream checks the external inventory rows against this list.

        A family whose every member is a `tools-external` adapter is one
        too. If it is missing, upstream's own check fails, and an operator
        who selects the family in a build without the feature gets no
        warning that this build cannot carry it.
        """
        families, _ = self.family_table()
        upstream = self.upstream_compilations(
            (build.ROOT / "crates/zeroclaw-tools/src/inventory.rs").read_text()
        )
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            self.copy_inputs(source)
            build.project_enact_family_inventory(source)
            listed = set(
                re.findall(
                    r'"([^"]+)"',
                    re.search(
                        r"pub const EXTERNAL_TOOL_NAMES: &\[&str\] = &\[(.*?)\];",
                        (source / "crates/zeroclaw-config/src/builtin_tools.rs").read_text(),
                        re.S,
                    ).group(1),
                )
            )
        external_families = {
            family
            for family, members in families.items()
            if members and all(upstream.get(member) == "External" for member in members)
        }
        self.assertTrue(external_families, "no family collects the external adapters")
        for family in external_families:
            self.assertIn(family, listed)
        # The family rows and the canonical list have to agree, or upstream's
        # "the external compilation rows match the canonical adapter names"
        # check fails.
        self.assertEqual(
            {name for name, group in upstream.items() if group == "External"} | external_families,
            listed,
        )

    def tool_properties(self, source_text, tool_name):
        """The parameter properties a tool's schema declares."""
        match = re.search(
            rf'fn name\(&self\) -> &str \{{\s*"{re.escape(tool_name)}"',
            source_text,
            re.S,
        )
        if not match:
            return None
        tail = source_text[match.end():]
        schema = tail.find("fn parameters_schema")
        if schema == -1:
            return None
        body = tail[schema:]
        start = body.find('"properties"')
        if start == -1:
            return set()
        brace = body.find("{", start)
        if brace == -1:
            return set()
        depth = 0
        end = brace
        for index in range(brace, len(body)):
            if body[index] == "{":
                depth += 1
            elif body[index] == "}":
                depth -= 1
                if depth == 0:
                    end = index
                    break
        return set(re.findall(r'"([a-z_]+)":\s*\{', body[brace:end]))

    def test_the_matrix_policy_names_the_arguments_the_tools_take(self):
        """The policy's field list has to match the registered tools.

        Upstream's entry is dormant there, because those tools are never in
        its default registry. The fork registers them, so the entry became
        live — and upstream's `skill_view` takes a `slug`, not a `name`.
        """
        skills = (build.ROOT / "crates/zeroclaw-runtime/src/tools/skill_manage.rs").read_text()
        read_skill = (
            build.ROOT / "crates/zeroclaw-runtime/src/tools/read_skill.rs"
        ).read_text()
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            orchestrator = source / "crates/zeroclaw-channels/src/orchestrator/mod.rs"
            orchestrator.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(
                build.ROOT / "crates/zeroclaw-channels/src/orchestrator/mod.rs", orchestrator
            )
            build.project_enact_matrix_safe_policy(source)
            projected = orchestrator.read_text()
        policy = {}
        for tools, fields in re.findall(
            r"\(&\[([^\]]*)\],\s*&\[([^\]]*)\]\),", projected
        ):
            names = {name.strip().strip('"') for name in tools.split(",") if name.strip()}
            wanted = {name.strip().strip('"') for name in fields.split(",") if name.strip()}
            for name in names:
                policy[name] = wanted

        def fields_of(tool):
            return policy.get(tool)

        for tool, source_text in (
            ("read_skill", read_skill),
            ("skill_view", skills),
            ("skills_list", skills),
            ("skill_manage", skills),
        ):
            with self.subTest(tool=tool):
                properties = self.tool_properties(source_text, tool)
                self.assertIsNotNone(properties, f"{tool} has no schema in the tree")
                declared = fields_of(tool)
                self.assertIsNotNone(declared, f"{tool} is not in the Matrix policy")
                self.assertTrue(
                    declared.issubset(properties),
                    f"the Matrix policy names {sorted(declared - properties)} for {tool}, "
                    f"which it does not accept; it accepts {sorted(properties)}",
                )

    def test_the_scope_filter_consults_the_family_table(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            relatives = (
                "crates/zeroclaw-api/src/tool.rs",
                "crates/zeroclaw-runtime/src/tools/scoped.rs",
                "crates/zeroclaw-runtime/src/tools/mod.rs",
                "crates/zeroclaw-runtime/src/tools/delegate.rs",
                "crates/zeroclaw-runtime/src/tools/skill_tool.rs",
                "crates/zeroclaw-tools/src/tool_search.rs",
                "crates/zeroclaw-runtime/src/agent/agent.rs",
                "crates/zeroclaw-gateway/src/dashboard_core.rs",
            )
            for relative in relatives:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_enact_family_scope_filter(source)
            api, scoped, runtime_tools, delegate_tools, skill_tools, tool_search, agent, gateway = (
                (source / relative).read_text() for relative in relatives
            )
        self.assertIn("crate::tools::families::family_by_name(name)", scoped)
        self.assertIn(
            ".any(|member| tool_allowed_in_context(member, exclude_memory, acp_delivery))",
            scoped,
        )
        self.assertIn("!tool.requires_unrestricted_principal()", scoped)
        self.assertIn("pub(crate) fn retain_projected(", scoped)
        self.assertIn("restricted_principal_variant", api)
        self.assertIn("self.0.restricted_principal_variant()", runtime_tools)
        self.assertEqual(delegate_tools.count("self.inner.restricted_principal_variant()"), 2)
        self.assertIn("self.target_tool.restricted_principal_variant()", skill_tools)
        self.assertIn("tool_name: self.tool_name.clone()", skill_tools)
        self.assertIn("locked_args: self.locked_args.clone()", skill_tools)
        self.assertIn("restricted_skill_family_preserves_alias_and_locked_action", skill_tools)
        self.assertIn("selected == \"mcp\"", tool_search)
        self.assertIn("canonical_mcp_selector_admits_capabilities", tool_search)
        self.assertIn("tool.restricted_principal_variant()", agent)
        self.assertIn("self.tools.retain_projected", agent)
        self.assertIn("built.unfiltered_tool_arcs = tools.to_vec()", runtime_tools)
        self.assertIn("family.members.iter().any(|member|", agent)
        self.assertIn('offered_actions(&fixture._model, marker, "sessions").await', gateway)
        self.assertIn('vec!["current"]', gateway)
        self.assertIn("caller_scope_wraps_delegate_execution", scoped)
        self.assertIn("memory_free_assembly_scopes_delegate_without_selector", scoped)
        self.assertIn("scope_caller_tool", scoped)
        self.assertNotIn("if caller_allowed.is_none()", scoped)
        self.assertIn("enact_project_supplied_families", scoped)
        self.assertIn("current_delegate_caller_tool_selector", delegate_tools)
        self.assertIn("current_delegate_caller_memory_free", delegate_tools)
        self.assertIn("exclude_memory: caller_memory_free", delegate_tools)
        self.assertIn("caller_allowed: caller_scope.as_deref()", delegate_tools)
        self.assertIn("spawned_caller_context_preserves_selector_and_memory_policy", delegate_tools)
        self.assertGreaterEqual(
            delegate_tools.count("Box::pin(scope_delegate_caller_context("), 2
        )
        self.assertIn("parent_caller_tool_selector.clone()", delegate_tools)
        self.assertIn("Arc::from(tool.restricted_principal_variant()?)", delegate_tools)
        self.assertIn("registry.retain_projected", delegate_tools)
        families = (build.ROOT / "overlays/enact/native/tools/families.rs").read_text()
        self.assertIn(".any(|action| action.tool.requires_unrestricted_principal())", families)

    def test_early_tool_factory_return_registers_the_canonical_families(self):
        relative = "crates/zeroclaw-runtime/src/tools/mod.rs"
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(build.ROOT / relative, target)
            build.project_enact_family_early_return(source)
            projected = target.read_text()

        early_return = projected.index("Match the normal registry path before returning")
        return_result = projected.index("return Ok(AllToolsResult {", early_return)
        self.assertLess(early_return, return_result)
        self.assertIn("build_all(&tool_arcs)", projected[early_return:return_result])
        self.assertIn("&root_config.tools", projected[early_return:return_result])

    def test_provider_request_projection_hides_members_and_preserves_runtime_specs(self):
        relative = "crates/zeroclaw-runtime/src/agent/turn/tool_specs.rs"
        original = (build.ROOT / relative).read_text()
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(build.ROOT / relative, target)
            build.project_enact_family_request_specs(source)
            projected = target.read_text()

        self.assertNotEqual(projected, original, "family projection must change provider specs")
        self.assertIn(".filter(|family| {", projected)
        self.assertIn("hidden_builtin_names.extend(activated_tools.hidden_builtin_names());", projected)
        self.assertIn("family_request_projection_hides_members_but_keeps_them_callable", projected)
        self.assertIn("specs.known_tool_names.contains(*member)", projected)
        # Preparing the same source twice is drift, not a silent no-op.
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(projected)
            with self.assertRaisesRegex(RuntimeError, "Upstream overlay drift"):
                build.project_enact_family_request_specs(source)

    def test_contextual_mcp_family_is_built_after_its_policy_gate(self):
        relative = "crates/zeroclaw-runtime/src/tools/scoped.rs"
        original = (build.ROOT / relative).read_text()
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(build.ROOT / relative, target)
            build.project_enact_contextual_mcp_family(source)
            projected = target.read_text()

        self.assertNotEqual(projected, original, "MCP family projection must change scoped assembly")
        self.assertIn('family_by_name("mcp")', projected)
        self.assertIn("family.members.contains(&tool.name())", projected)
        self.assertIn("security.is_tool_excluded(family.name)", projected)
        self.assertIn("name == family.name || name == tool.name()", projected)
        self.assertIn("FamilyTool::build(family, members)", projected)
        self.assertIn('eager_names.iter().any(|name| name == "mcp")', projected)
        self.assertIn('deferred_names.iter().any(|name| name == "mcp")', projected)
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(projected)
            with self.assertRaisesRegex(RuntimeError, "Upstream overlay drift"):
                build.project_enact_contextual_mcp_family(source)

    def test_delegate_expectations_use_the_canonical_sessions_family(self):
        relatives = (
            "crates/zeroclaw-runtime/src/tools/delegate.rs",
            "tests/fixtures/runtime-composition-consumer/src/lib.rs",
        )
        originals = {relative: (build.ROOT / relative).read_text() for relative in relatives}
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in relatives:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_enact_family_delegate_tests(source)
            projected = {relative: (source / relative).read_text() for relative in relatives}

        for relative in relatives:
            self.assertNotEqual(
                projected[relative], originals[relative],
                f"family test projection must change assertions in {relative}",
            )
        self.assertIn('for name in ["sessions", "spawn_subagent"]', projected[relatives[0]])
        self.assertIn('offered_tool(&requests[1], "sessions")', projected[relatives[0]])
        self.assertIn(
            'first.iter().any(|name| name == "sessions")',
            projected[relatives[1]],
        )
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in relatives:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(projected[relative])
            with self.assertRaisesRegex(RuntimeError, "Upstream overlay drift"):
                build.project_enact_family_delegate_tests(source)


class ProviderProxyTestIsolationTests(unittest.TestCase):
    def test_local_openai_request_fixtures_share_the_proxy_test_lock(self):
        relative = "crates/zeroclaw-providers/src/openai.rs"
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / relative
            target.parent.mkdir(parents=True)
            shutil.copy2(build.ROOT / relative, target)
            build.project_enact_provider_proxy_test_isolation(source)
            projected = target.read_text()

        self.assertEqual(
            projected.count("let _proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;"),
            4,
        )
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            target = source / relative
            target.parent.mkdir(parents=True)
            target.write_text(projected)
            with self.assertRaisesRegex(RuntimeError, "Upstream overlay drift"):
                build.project_enact_provider_proxy_test_isolation(source)


class HookInterfaceInstallTests(unittest.TestCase):
    """The hook interface installs once, whatever the crate path is.

    A committed tree keeps `crates/zeroclaw-plugins/wit` as a symlink to the
    package root, so the two names are one file. A working-tree preparation
    dereferences it into a real directory. Both have to work.
    """

    overlay_wit = "overlays/enact/native/plugins/hook.wit"

    def test_symlinked_crate_path_installs_one_interface(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            (source / "wit/v0").mkdir(parents=True)
            (source / "crates/zeroclaw-plugins").mkdir(parents=True)
            (source / "crates/zeroclaw-plugins/wit").symlink_to(
                Path("../../wit"), target_is_directory=True
            )
            build.install_enact_hook_wit(source)
            package = source / "wit/v0/hook.wit"
            self.assertTrue(package.is_file())
            self.assertEqual(package.read_bytes(), (build.ROOT / self.overlay_wit).read_bytes())
            crate = source / "crates/zeroclaw-plugins/wit/v0/hook.wit"
            self.assertEqual(crate.resolve(), package.resolve())

    def test_distinct_crate_path_installs_both_copies(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            (source / "wit/v0").mkdir(parents=True)
            (source / "crates/zeroclaw-plugins/wit/v0").mkdir(parents=True)
            build.install_enact_hook_wit(source)
            self.assertTrue((source / "wit/v0/hook.wit").is_file())
            self.assertTrue((source / "crates/zeroclaw-plugins/wit/v0/hook.wit").is_file())

    def test_a_conflicting_upstream_interface_fails_closed(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            (source / "wit/v0").mkdir(parents=True)
            (source / "wit/v0/hook.wit").write_text("package zeroclaw:plugin@0.1.0;\n")
            with self.assertRaisesRegex(RuntimeError, "differs from the overlay"):
                build.install_enact_hook_wit(source)

    def test_installing_twice_is_not_drift(self):
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            (source / "wit/v0").mkdir(parents=True)
            (source / "crates/zeroclaw-plugins/wit/v0").mkdir(parents=True)
            build.install_enact_hook_wit(source)
            build.install_enact_hook_wit(source)


class ReleaseTests(unittest.TestCase):
    def test_fixed_encoded_remap_flag_preserves_spaces_without_inherited_flags(self):
        home = "/private/fixture builder/home with spaces"
        with patch.dict(build.os.environ, {"HOME": home, "RUSTFLAGS": "--cfg inherited_flag",
                                         "CARGO_ENCODED_RUSTFLAGS": "--cfg\x1finherited_flag",
                                         "PROVIDER_CREDENTIAL": "fixture-marker"}):
            env = build.native_build_environment("/tools/cargo", "/tools/rustc",
                                                {"forkCommit": "1" * 40, "forkWorkingTreeDirty": False})
        self.assertEqual(env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f"),
                         [f"--remap-path-prefix={home}=/build/home"])
        self.assertNotIn("RUSTFLAGS", env)
        self.assertNotIn("PROVIDER_CREDENTIAL", env)
        self.assertNotIn("inherited_flag", env["CARGO_ENCODED_RUSTFLAGS"])

    def test_private_home_binary_scan_fails_closed_without_printing_path(self):
        home = "/private/fixture builder/home with spaces"
        with tempfile.TemporaryDirectory() as name:
            binary = Path(name) / "binary-fixture"
            binary.write_bytes(b"prefix" + home.encode() + b"/source.rs")
            with self.assertRaisesRegex(RuntimeError, "publication refused") as caught:
                build.require_private_home_absent(binary, home)
            self.assertNotIn(home, str(caught.exception))
            binary.write_bytes(b"/build/home/source.rs")
            build.require_private_home_absent(binary, home)
            actual_home = Path(name) / "actual fixture home"
            actual_home.mkdir()
            alias_home = Path(name) / "alias fixture home"
            alias_home.symlink_to(actual_home, target_is_directory=True)
            binary.write_bytes(str(actual_home.resolve()).encode() + b"/source.rs")
            with self.assertRaisesRegex(RuntimeError, "publication refused") as caught:
                build.require_private_home_absent(binary, str(alias_home))
            self.assertNotIn(str(actual_home), str(caught.exception))
            self.assertNotIn(str(alias_home), str(caught.exception))

    def test_upstream_base_does_not_follow_fork_head(self):
        fork = "1" * 40
        with patch.object(build, "run"), patch.object(build.subprocess, "check_output",
                                                      side_effect=[fork + "\n", ""]):
            provenance = build.source_provenance()
        self.assertEqual(provenance["forkCommit"], fork)
        self.assertEqual(provenance["upstreamCommit"], build.release_manifest()["upstreamCommit"])
        self.assertNotEqual(provenance["upstreamCommit"], fork)
        with patch.object(build, "source_provenance", return_value={"forkWorkingTreeDirty": True}):
            with self.assertRaisesRegex(RuntimeError, "clean committed"):
                build.require_committed_release()

    def test_archive_inventory_checksums_and_binary_tamper_rejection(self):
        brand = build.manifest()
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            artifact = root / "payload"
            artifact.mkdir()
            binary = artifact / brand["command"]
            binary.write_bytes(b"archive-format-test-fixture\n")
            binary.chmod(0o755)
            metadata = {"binary": {"sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                                   "sizeBytes": binary.stat().st_size, "target": "aarch64-apple-darwin"}}
            (artifact / "manifest.json").write_text(json.dumps(metadata))
            release = build.release_manifest()
            (artifact / "RELEASE.json").write_text(json.dumps({
                "distribution": {"version": release["version"],
                                 "tag": f"{brand['command']}-v{release['version']}"},
                "manifestSha256": hashlib.sha256((artifact / "manifest.json").read_bytes()).hexdigest()}))
            source = root / "source"
            (source / "web/public").mkdir(parents=True)
            for filename in ("LICENSE-APACHE", "LICENSE-MIT", "NOTICE"):
                shutil.copy2(build.ROOT / filename, source / filename)
                shutil.copy2(source / filename, artifact / filename)
            (artifact / "UPSTREAM.txt").write_text("Archive format test attribution fixture\n")
            for filename in (brand["logo"], brand["logoDark"]):
                shutil.copy2(build.ROOT / "web/public" / filename, source / "web/public" / filename)
            with patch.object(build, "SOURCE", source):
                archive = build.package_native_release(brand, artifact, root)
                with tarfile.open(archive) as stream:
                    for member in stream.getmembers():
                        self.assertEqual((member.uid, member.gid, member.uname, member.gname), (0, 0, "", ""))
                        self.assertFalse(set(member.pax_headers) & {"uid", "gid", "uname", "gname",
                            "SCHILY.uid", "SCHILY.gid", "SCHILY.uname", "SCHILY.gname"})
                    members = {Path(member.name).relative_to(archive.name.removesuffix(".tar.gz")).as_posix(): member
                               for member in stream.getmembers() if member.isfile()}
                    self.assertEqual(set(members), {brand["command"], "manifest.json", "RELEASE.json", "LICENSE-APACHE",
                        "LICENSE-MIT", "NOTICE", "UPSTREAM.txt", "INSTALL.txt", "SHA256SUMS",
                        f"branding/{brand['logo']}", f"branding/{brand['logoDark']}"})
                    self.assertEqual(members[brand["command"]].mode & 0o777, 0o755)
                    self.assertEqual(json.load(stream.extractfile(members["manifest.json"])), metadata)
                    checksums = stream.extractfile(members["SHA256SUMS"]).read().decode().splitlines()
                    for line in checksums:
                        expected, filename = line.split("  ", 1)
                        self.assertEqual(hashlib.sha256(stream.extractfile(members[filename]).read()).hexdigest(), expected)
                self.assertEqual((root / "SHA256SUMS").read_text(),
                                 f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
                binary.write_bytes(b"tampered")
                with self.assertRaisesRegex(RuntimeError, "does not match"):
                    build.package_native_release(brand, artifact, root)

    def test_owner_pax_fields_are_removed(self):
        member = tarfile.TarInfo("ownership-fixture")
        member.uid, member.gid, member.uname, member.gname = 501, 20, "builder-fixture", "group-fixture"
        member.pax_headers = {"uid": "501", "uname": "builder-fixture", "SCHILY.gid": "20", "mtime": "1.25"}
        result = build.public_tar_member(member)
        self.assertEqual((result.uid, result.gid, result.uname, result.gname), (0, 0, "", ""))
        self.assertEqual(result.pax_headers, {"mtime": "1.25"})

    def test_canonical_multiline_svg_dimensions_and_tray_indicator(self):
        brand = build.manifest()
        for filename in (brand["logo"], brand["logoDark"]):
            original = (build.ROOT / "web/public" / filename).read_text()
            parsed = ET.fromstring(original)
            rendered = ET.fromstring(build.desktop_svg(original, "#22c55e"))
            self.assertEqual(rendered.attrib["viewBox"], parsed.attrib["viewBox"])
            self.assertEqual((rendered.attrib["width"], rendered.attrib["height"]), ("1024", "1024"))
            self.assertEqual(rendered.attrib["preserveAspectRatio"], "xMidYMid meet")
            x, y, width, height = map(float, rendered.attrib["viewBox"].split())
            indicator = rendered[-1]
            self.assertGreater(float(indicator.attrib["cx"]), x)
            self.assertLess(float(indicator.attrib["cx"]) + float(indicator.attrib["r"]), x + width)
            self.assertLess(float(indicator.attrib["cy"]) + float(indicator.attrib["r"]), y + height)


if __name__ == "__main__":
    unittest.main()
