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
                self.assertEqual((source / path).read_bytes(), content)
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
            target = source / "crates/zeroclaw-runtime/src/tools/scoped.rs"
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(build.ROOT / "crates/zeroclaw-runtime/src/tools/scoped.rs", target)
            build.project_enact_family_scope_filter(source)
            projected = target.read_text()
        self.assertIn("crate::tools::families::family_by_name(name)", projected)
        self.assertIn(
            ".any(|member| tool_allowed_in_context(member, exclude_memory, acp_delivery))",
            projected,
        )


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
