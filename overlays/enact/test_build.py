"""Boundary checks for the staged distribution's environment namespace."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import shutil
import hashlib
import json
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
                 "crates/zeroclaw-runtime/src/rpc/types.rs",
                 "crates/zeroclaw-runtime/src/agent/agent.rs"]
        with tempfile.TemporaryDirectory() as name:
            source = Path(name)
            for relative in paths:
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(build.ROOT / relative, target)
            build.project_session_tools(source)
            dispatch = (source / paths[0]).read_text()
            self.assertIn('(Method::SessionTools, "session/tools")', dispatch)
            self.assertIn('| M::SessionTools\n', dispatch)
            self.assertIn('| Method::SessionTools\n', dispatch)
            handler = dispatch.split('    async fn handle_session_tools(', 1)[1].split(
                '    async fn handle_session_state(', 1)[0]
            for guard in ('authorize_session_owner', 'capture_session_access',
                          'revalidate_admitted_session', 'ensure_session_incarnation'):
                self.assertIn(guard, handler)
            self.assertIn('agent.tool_names()', handler)
            self.assertNotIn('self.ctx.config', handler)
            self.assertNotIn('provider', handler)
            self.assertNotIn('#[cfg(test)]\n    pub fn tool_names', (source / paths[2]).read_text())
            with self.assertRaisesRegex(RuntimeError, 'Upstream overlay drift'):
                build.project_session_tools(source)


class ToolResultStatusProjectionTests(unittest.TestCase):
    def test_every_known_producer_preserves_authority_and_preexecution_is_unknown(self):
        files = ["crates/zeroclaw-api/src/agent.rs",
                 "crates/zeroclaw-runtime/src/agent/tool_execution.rs",
                 "crates/zeroclaw-runtime/src/agent/turn/events.rs",
                 "crates/zeroclaw-runtime/src/agent/turn/stream_consume.rs",
                 "crates/zeroclaw-runtime/src/rpc/dispatch.rs",
                 "crates/zeroclaw-runtime/src/rpc/types.rs",
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
            (artifact / "RELEASE.json").write_text(json.dumps({
                "distribution": {"version": "0.8.6-enact.2", "tag": "enact-agent-v0.8.6-enact.2"},
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
