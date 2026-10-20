#!/usr/bin/env python3
"""Materialize the Enact distribution without editing upstream runtime sources."""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import struct
import tarfile
import tomllib
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "target/enact"
SOURCE = OUT / "source"
PUBLIC_BUILD_HOME = "/build/home"


def release_manifest():
    release = json.loads((ROOT / "overlays/enact/release.json").read_text())
    if not re.fullmatch(r"\d+\.\d+\.\d+-enact\.\d+", release["version"]):
        raise ValueError("Distribution version must use X.Y.Z-enact.N")
    if not re.fullmatch(r"[0-9a-f]{40}", release["upstreamCommit"]):
        raise ValueError("Upstream base must be a full Git commit")
    return release


def source_provenance():
    release = release_manifest()
    upstream = release["upstreamCommit"]
    run("git", "merge-base", "--is-ancestor", upstream, "HEAD")
    fork = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=ROOT, text=True))
    return {"upstreamCommit": upstream, "upstreamRepository": release["upstreamRepository"],
            "forkRepository": release["forkRepository"],
            "forkCommit": fork, "forkWorkingTreeDirty": dirty}


def require_committed_release():
    provenance = source_provenance()
    if provenance["forkWorkingTreeDirty"]:
        raise RuntimeError("Release requires a clean committed worktree")
    origin = subprocess.check_output(["git", "remote", "get-url", "origin"], cwd=ROOT, text=True).strip()
    if origin != provenance["upstreamRepository"]:
        raise RuntimeError("Release requires origin to remain the configured upstream repository")
    return provenance


def source_fingerprint(source):
    digest = hashlib.sha256()
    for path in sorted(source.rglob("*")):
        if path.is_file():
            relative = path.relative_to(source).as_posix().encode()
            content = path.read_bytes()
            digest.update(len(relative).to_bytes(8, "big"))
            digest.update(relative)
            digest.update(len(content).to_bytes(8, "big"))
            digest.update(content)
    return digest.hexdigest()


def run(*args, cwd=ROOT, env=None):
    subprocess.run(args, cwd=cwd, env=env, check=True)


def replace_required(path, old, new):
    text = path.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f"Upstream overlay drift: expected exactly one {old!r} in {path}")
    path.write_text(text.replace(old, new))


def manifest():
    brand = json.loads((ROOT / "overlays/enact/brand.json").read_text())
    if not re.fullmatch(r"[a-z][a-z0-9-]*", brand["command"]):
        raise ValueError("Brand command must be a shell-safe executable name")
    if not re.fullmatch(r"[A-Za-z][A-Za-z0-9 ]*", brand["name"]):
        raise ValueError("Brand name must contain only letters, digits and spaces")
    return brand


def project_display(text, brand):
    text = text.replace("ZeroClaw", brand["name"])
    return re.sub(r"(?<![\w./-])zeroclaw(?=[ `])", brand["command"], text)


# Skip comments and character literals; project only Rust string contents.
# This covers raw/byte/C strings as well as env!, concat! and format! inputs.
_RUST_TOKEN = re.compile(
    r'//|/\*|(?:br|cr|r)(?P<hashes>\#{0,255})"|'
    r'"(?:\\[\s\S]|[^"\\])*"|'
    r"(?:b)?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'"
)
_BLOCK_DELIMITER = re.compile(r"/\*|\*/")


def project_environment_literals(text):
    """Use the distribution env namespace without renaming Rust identities."""
    pieces = []
    cursor = copied = 0
    while match := _RUST_TOKEN.search(text, cursor):
        token = match.group()
        end = match.end()
        if token == "//":
            newline = text.find("\n", end)
            cursor = len(text) if newline == -1 else newline + 1
            continue
        if token == "/*":
            depth = 1
            while depth:
                delimiter = _BLOCK_DELIMITER.search(text, end)
                if delimiter is None:
                    raise RuntimeError("Unterminated Rust block comment")
                depth += 1 if delimiter.group() == "/*" else -1
                end = delimiter.end()
            cursor = end
            continue
        if match.group("hashes") is not None:
            delimiter = '"' + match.group("hashes")
            closing = text.find(delimiter, end)
            if closing == -1:
                raise RuntimeError("Unterminated Rust raw string")
            end = closing + len(delimiter)
            token = text[match.start():end]
        if token.startswith('"') or match.group("hashes") is not None:
            projected = token.replace("ZEROCLAW_", "ENACT_AGENT_")
            if projected != token:
                pieces.extend((text[copied:match.start()], projected))
                copied = end
        cursor = end
    pieces.append(text[copied:])
    return "".join(pieces)


def project_environment(source):
    # Compile-time producers and consumers must move together (buildinfo emits
    # VERSION_ENV; env! reads it). Include every Rust crate and build script.
    for path in source.rglob("*.rs"):
        original = path.read_text()
        projected = project_environment_literals(original)
        if projected != original:
            path.write_text(projected)


def project_session_tools(source):
    """Expose only the instantiated session registry through existing RPC guards."""
    dispatch = source / "crates/zeroclaw-runtime/src/rpc/dispatch.rs"
    for old, new in (
        ("    SessionState,\n", "    SessionState,\n    SessionTools,\n"),
        ('        (Method::SessionState, "session/state"),\n',
         '        (Method::SessionState, "session/state"),\n'
         '        (Method::SessionTools, "session/tools"),\n'),
        ("            | M::SessionState\n", "            | M::SessionState\n            | M::SessionTools\n"),
        ("                | Method::SessionState\n",
         "                | Method::SessionState\n                | Method::SessionTools\n"),
        ("            Method::SessionState => self.handle_session_state(params).await,\n",
         "            Method::SessionState => self.handle_session_state(params).await,\n"
         "            Method::SessionTools => self.handle_session_tools(params).await,\n"),
    ):
        replace_required(dispatch, old, new)
    handler = '''    async fn handle_session_tools(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        let authorized = self
            .authorize_session_owner(&req.session_id, Method::SessionTools)
            .await?;
        let expected_generation = self.capture_session_access(&req.session_id).await?;
        let agent = self.ctx.sessions.get_agent(&req.session_id).await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Live session not found"))?;
        let tools = {
            let agent = agent.lock().await;
            agent.tool_names().into_iter().map(str::to_string).collect()
        };
        self.revalidate_admitted_session(&req.session_id, authorized.as_ref()).await?;
        self.ensure_session_incarnation(&req.session_id, expected_generation).await?;
        to_result(SessionToolsResult { session_id: req.session_id, tools })
    }

'''
    replace_required(dispatch, "    async fn handle_session_state(&self, params: &Value) -> RpcResult {",
                     handler + "    async fn handle_session_state(&self, params: &Value) -> RpcResult {")
    replace_required(source / "crates/zeroclaw-runtime/src/rpc/types.rs",
                     "rpc_type! {\n    pub struct SessionStateResult {",
                     "rpc_type! {\n    pub struct SessionToolsResult {\n"
                     "        pub session_id: String,\n        pub tools: Vec<String>,\n    }\n}\n\n"
                     "rpc_type! {\n    pub struct SessionStateResult {")
    # Canonical names come from the assembled Agent registry, after MCP startup
    # and tool-policy filtering. No config-derived inventory or cached copy.
    replace_required(source / "crates/zeroclaw-runtime/src/agent/agent.rs",
                     "    #[cfg(test)]\n    pub fn tool_names(&self) -> Vec<&str> {",
                     "    pub fn tool_names(&self) -> Vec<&str> {")


def project_tool_result_status(source):
    """Preserve factual execution status; provider-preexecuted status is unknown."""
    replace_required(source / "crates/zeroclaw-api/src/agent.rs",
                     "        output: String,\n        /// Typed metadata for a file-producing tool",
                     "        output: String,\n"
                     "        /// Actual local execution outcome; provider-preexecuted outcomes are unknown.\n"
                     "        success: Option<bool>,\n        /// Typed metadata for a file-producing tool")
    for relative, old, new in (
        ("agent/tool_execution.rs", "                output: scrub_credentials(&out.output),",
         "                output: scrub_credentials(&out.output),\n                success: Some(out.success),"),
        ("agent/turn/events.rs", "            output: scrub_credentials(&outcome.output),",
         "            output: scrub_credentials(&outcome.output),\n            success: Some(outcome.success),"),
        ("agent/turn/stream_consume.rs", "                            output,\n                            artifact: None,",
         "                            output,\n                            success: None,\n                            artifact: None,"),
    ):
        replace_required(source / "crates/zeroclaw-runtime/src" / relative, old, new)
    dispatch = source / "crates/zeroclaw-runtime/src/rpc/dispatch.rs"
    replace_required(dispatch,
                     "            output,\n            artifact: _,\n        } => SessionUpdateEvent::ToolResult {",
                     "            output,\n            success,\n            artifact: _,\n        } => SessionUpdateEvent::ToolResult {")
    replace_required(dispatch, "            raw_output: output.clone(),",
                     "            raw_output: output.clone(),\n            success: *success,")
    replace_required(source / "crates/zeroclaw-runtime/src/rpc/types.rs",
                     "        raw_output: String,\n    },\n    ApprovalRequest {",
                     "        raw_output: String,\n        #[serde(default)]\n"
                     "        success: Option<bool>,\n    },\n    ApprovalRequest {")
    # The direct ACP transport does not consume the added native RPC status.
    replace_required(source / "crates/zeroclaw-channels/src/orchestrator/acp_server.rs",
                     "            output,\n            artifact,\n        } => {",
                     "            output,\n            artifact,\n            ..\n        } => {")
    # Existing synthetic unit fixtures had no status authority. Keep it unknown,
    # and fail on constructor drift rather than silently skipping test sources.
    for relative, expected in (("crates/zeroclaw-runtime/src/rpc/dispatch.rs", 4),
                               ("crates/zeroclaw-runtime/src/rpc/turn.rs", 1),
                               ("crates/zeroclaw-channels/src/orchestrator/acp_server.rs", 10)):
        path = source / relative
        text, count = re.subn(r"(TurnEvent::ToolResult \{\n)([ \t]+)(?=id:)",
                              r"\1\2success: None,\n\2", path.read_text())
        if count != expected:
            raise RuntimeError(f"Upstream overlay drift: expected {expected} synthetic ToolResult constructors in {path}, got {count}")
        path.write_text(text)
    with dispatch.open("a") as stream:
        stream.write('''
#[cfg(test)]
mod enact_tool_status_tests {
    #[test]
    fn native_status_preserves_true_false_and_unknown_provider_preexecution() {
        for success in [Some(true), Some(false), None] {
            let event = super::TurnEvent::ToolResult {
                id: "status-fixture".into(), name: "fixture".into(),
                output: "Text never determines status".into(), artifact: None, success,
            };
            let notification = super::notification_for_turn_event("session", &event).unwrap();
            let wire: serde_json::Value = serde_json::from_str(&notification).unwrap();
            assert_eq!(wire["params"]["success"], serde_json::json!(success));
        }
    }
}
''')


def project_clap_docs(brand):
    # Rust doc comments and Clap help attributes are compile-time help sources.
    # Other code, identifiers, string arguments and compatibility paths stay put.
    for path in (SOURCE / "src/main.rs", SOURCE / "src/lib.rs"):
        lines = path.read_text().splitlines(keepends=True)
        in_attribute = False
        for index, line in enumerate(lines):
            stripped = line.lstrip()
            if stripped.startswith(("#[command(", "#[arg(", "#[clap(")):
                in_attribute = True
            if stripped.startswith("///") or in_attribute:
                lines[index] = project_display(line, brand)
            if in_attribute and ")]" in line:
                in_attribute = False
        path.write_text("".join(lines))


def prepare(cargo="cargo", committed=False, revision="HEAD"):
    brand = manifest()
    if SOURCE.exists():
        shutil.rmtree(SOURCE)
    SOURCE.mkdir(parents=True)
    if committed:
        # Release inputs come from the approved commit, never ignored web output
        # or files changed after the clean-worktree admission check.
        archive = subprocess.check_output(["git", "archive", "--format=tar", revision], cwd=ROOT)
        with tarfile.open(fileobj=io.BytesIO(archive)) as stream:
            stream.extractall(SOURCE, filter="data")
    else:
        # Development snapshots include working-tree edits and new overlay files.
        files = subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=ROOT
        ).decode().split("\0")
        for name in filter(None, files):
            source = ROOT / name
            if not source.is_file():
                continue
            dest = SOURCE / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest)
    # These ignored outputs are prerequisites produced by the normal web build.
    if not committed and (ROOT / "web/dist/index.html").is_file():
        shutil.copytree(ROOT / "web/dist", SOURCE / "web/dist", dirs_exist_ok=True)

    # Retain Fluent keys, placeholders, URLs, config paths and crate identities.
    # Only the existing product literal and bare command examples are projected.
    for catalogue in (SOURCE / "crates/zeroclaw-runtime/locales").glob("*/*.ftl"):
        text = project_display(catalogue.read_text(), brand)
        catalogue.write_text(text)
    english = SOURCE / "crates/zeroclaw-runtime/locales/en/cli.ftl"
    with english.open("a") as stream:
        stream.write(
            f"\n# Generated from overlays/enact/brand.json.\n"
            f"cli-enact-agent-name = {brand['name']}\n"
            f"cli-onboard-about = Deprecated. Use `{brand['command']} quickstart`. Any flags error.\n"
            f"cli-enact-agent-identity = You are {brand['name']}, a fast and efficient AI assistant built in Rust. Be helpful, concise, and direct.\n"
            "cli-enact-updates-disabled = This branded distribution cannot install upstream release binaries. Pull upstream changes into your branding branch and rebuild with python3 overlays/enact/build.py package.\n"
            "cli-enact-desktop-build = Build the branded desktop app with python3 overlays/enact/build.py desktop. Upstream desktop downloads do not include this overlay.\n"
        )
    replace_required(SOURCE / "src/main.rs", '#[command(name = "zeroclaw")]',
                     f'#[command(name = "{brand["command"]}", bin_name = "{brand["command"]}")]')
    main_source = SOURCE / "src/main.rs"
    replace_required(SOURCE / "crates/zeroclaw-config/src/sections.rs",
                     "Pick which chat platforms ZeroClaw should listen on.",
                     f"Pick which chat platforms {brand['name']} should listen on.")
    for prefix in ("/Applications/", "Applications/"):
        replace_required(main_source, f'{prefix}ZeroClaw.app/Contents/MacOS/ZeroClaw',
                         f'{prefix}{brand["name"]}.app/Contents/MacOS/zeroclaw-desktop')
    replace_required(main_source, "            if do_install {\n",
                     '            if do_install && !env!("ENACT_AGENT_BUILD").is_empty() {\n'
                     '                bail!(t("cli-enact-desktop-build", "Build the branded desktop app from source."));\n'
                     '            }\n            if do_install {\n')
    name_lookup = 'crate::i18n::get_required_cli_string("cli-enact-agent-name")'
    replace_required(SOURCE / "crates/zeroclaw-runtime/src/agent/personality_templates/mod.rs",
                     'agent: "ZeroClaw".to_string(),', f"agent: {name_lookup},")
    replace_required(SOURCE / "crates/zeroclaw-runtime/src/rpc/dispatch.rs",
                     '.unwrap_or_else(|| "ZeroClaw".to_string()),',
                     f".unwrap_or_else(|| {name_lookup}),")
    replace_required(SOURCE / "crates/zeroclaw-runtime/src/rpc/dispatch.rs",
                     'assert_eq!(ctx.agent, "ZeroClaw");',
                     f'assert_eq!(ctx.agent, "{brand["name"]}");')
    # Existing catalogue assertions exercise the projected product and command.
    i18n = SOURCE / "crates/zeroclaw-runtime/src/i18n.rs"
    text = i18n.read_text()
    start = text.index("#[cfg(test)]\nmod tests")
    tests = text[start:].replace("ZeroClaw", brand["name"])
    tests = re.sub(r"(?<![\w./-])zeroclaw(?=[ `])", brand["command"], tests)
    i18n.write_text(text[:start] + tests)
    # Disk catalogues override built-ins. Project their source before Fluent
    # parsing so existing/fetched translations cannot restore upstream branding;
    # user-supplied Fluent arguments are never rewritten.
    replace_required(i18n, "            return Some(content);",
                     '            // Invariant: this static pattern contains no user input and is valid.\n'
                     '            let command = regex::Regex::new(r"(^|[^\\w./-])zeroclaw([ `])")\n'
                     '                .expect("static branding regex is valid");\n'
                     f'            return Some(command.replace_all(&content, "${{1}}{brand["command"]}${{2}}")\n'
                     f'                .into_owned().replace("ZeroClaw", "{brand["name"]}"));')
    daemon = SOURCE / "crates/zeroclaw-runtime/src/daemon/mod.rs"
    replace_required(daemon, 'assert!(starting.contains("ZeroClaw"));',
                     f'assert!(starting.contains("{brand["name"]}"));')
    replace_required(SOURCE / "crates/zeroclaw-runtime/src/agent/system_prompt.rs",
                     '"You are ZeroClaw, a fast and efficient AI assistant built in Rust. Be helpful, concise, and direct."\n            .to_string()',
                     'crate::i18n::get_required_cli_string("cli-enact-agent-identity")')
    # Both the CLI and gateway call this updater. Compile-time env is mandatory
    # so a manual Cargo build of staged source cannot silently omit protection.
    update_signature = "pub async fn run(target_version: Option<&str>, force: bool) -> Result<()> {"
    replace_required(SOURCE / "src/commands/update.rs", update_signature,
                     update_signature + '\n    if !env!("ENACT_AGENT_BUILD").is_empty() {\n'
                     '        #[cfg(feature = "agent-runtime")]\n'
                     '        bail!(get_required_cli_string("cli-enact-updates-disabled"));\n'
                     '        #[cfg(not(feature = "agent-runtime"))]\n'
                     '        bail!("Branded distributions must be rebuilt from source.");\n    }')

    # Tauri's package/binary identifiers stay stable. Only display surfaces vary.
    config_path = SOURCE / "apps/tauri/tauri.conf.json"
    config = json.loads(config_path.read_text())
    config["productName"] = brand["name"]
    # Local distribution builds use an ad-hoc identity, never a personal key.
    config["bundle"].setdefault("macOS", {})["signingIdentity"] = "-"
    for window in config["app"]["windows"]:
        window["title"] = brand["name"]
    config_path.write_text(json.dumps(config, indent=2) + "\n")
    for relative in ["lib.rs", "tray/menu.rs", "tray/icon.rs", "tray/mod.rs"]:
        path = SOURCE / "apps/tauri/src" / relative
        text = path.read_text()
        if "ZeroClaw" not in text:
            raise RuntimeError(f"Upstream overlay drift: no desktop brand in {path}")
        path.write_text(text.replace("ZeroClaw", brand["name"]))
    splash = SOURCE / "apps/tauri/splash/index.html"
    splash.write_text(splash.read_text().replace("ZeroClaw", brand["name"]))
    info = SOURCE / "apps/tauri/Info.plist"
    info.write_text(info.read_text().replace("ZeroClaw", brand["name"]))
    for name in (brand["logo"], brand["logoDark"]):
        logo = SOURCE / "web/public" / name
        if not logo.is_file():
            raise RuntimeError(f"Missing canonical logo: {logo}")
        shutil.copy2(logo, SOURCE / "apps/tauri/splash" / name)
    replace_required(splash, '<div class="mark">🦀</div>',
                     '<picture class="mark">'
                     f'<source media="(prefers-color-scheme: dark)" srcset="{brand["logoDark"]}" />'
                     f'<img src="{brand["logo"]}" width="52" height="52" style="object-fit: contain" alt="" />'
                     '</picture>')
    acp = SOURCE / "crates/zeroclaw-channels/src/orchestrator/acp_server.rs"
    replace_required(acp, '"title": "ZeroClaw ACP",', f'"title": "{brand["name"]}",')
    replace_required(acp, 'assert_eq!(result["agentInfo"]["title"], "ZeroClaw ACP");',
                     f'assert_eq!(result["agentInfo"]["title"], "{brand["name"]}");')
    project_clap_docs(brand)
    project_environment(SOURCE)
    project_session_tools(SOURCE)
    project_tool_result_status(SOURCE)
    run(cargo, "fmt", "--all", cwd=SOURCE)
    print(f"Prepared branded source: {SOURCE}")
    return brand


def desktop_svg(svg, indicator=None):
    """Put either canonical SVG aspect ratio inside the square icon canvas."""
    ET.register_namespace("", "http://www.w3.org/2000/svg")
    root = ET.fromstring(svg)
    x, y, width, height = map(float, root.attrib["viewBox"].split())
    if width <= 0 or height <= 0:
        raise ValueError("Canonical logo must have a positive SVG viewBox")
    root.set("width", "1024")
    root.set("height", "1024")
    root.set("preserveAspectRatio", "xMidYMid meet")
    if indicator:
        unit = min(width, height)
        ET.SubElement(root, "{http://www.w3.org/2000/svg}circle", {
            "cx": str(x + width * 0.82), "cy": str(y + height * 0.82),
            "r": str(unit * 0.14), "fill": indicator,
            "stroke": "#181425", "stroke-width": str(unit * 0.04),
        })
    return ET.tostring(root, encoding="unicode")


def desktop_icons(brand):
    """Render the canonical SVG with macOS's native image tools."""
    if not shutil.which("sips") or not shutil.which("iconutil"):
        raise RuntimeError("Native desktop icon generation currently requires macOS sips and iconutil.")
    icons = SOURCE / "apps/tauri/icons"
    svg = (SOURCE / "web/public" / brand["logo"]).read_text()
    vector = OUT / "enact-icon.svg"
    vector.write_text(desktop_svg(svg))
    master = OUT / "enact-icon.png"
    run("sips", "-s", "format", "png", str(vector), "--out", str(master))
    for path in icons.glob("*.png"):
        with path.open("rb") as stream:
            header = stream.read(24)
        width, height = struct.unpack(">II", header[16:24])
        source = master
        if path.name.startswith("tray-"):
            colors = {"idle": "#a78bfa", "working": "#22c55e", "error": "#ef4444", "disconnected": "#94a3b8"}
            state = path.stem.removeprefix("tray-")
            vector.write_text(desktop_svg(svg, colors[state]))
            source = OUT / f"{path.stem}.png"
            run("sips", "-s", "format", "png", str(vector), "--out", str(source))
        run("sips", "-z", str(height), str(width), str(source), "--out", str(path))
    shutil.copy2(SOURCE / "web/public" / brand["logo"], icons / "icon.svg")
    iconset = OUT / "enact.iconset"
    iconset.mkdir(exist_ok=True)
    for size in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            name = f"icon_{size}x{size}" + ("@2x" if scale == 2 else "") + ".png"
            run("sips", "-z", str(size * scale), str(size * scale), str(master), "--out", str(iconset / name))
    run("iconutil", "-c", "icns", str(iconset), "-o", str(icons / "icon.icns"))
    png = (iconset / "icon_128x128@2x.png").read_bytes()
    (icons / "icon.ico").write_bytes(struct.pack("<HHH", 0, 1, 1) + struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 22) + png)


def build(brand, debug=False):
    if not (SOURCE / "web/dist/index.html").is_file():
        raise RuntimeError("Missing web/dist. Run npm run build in web before building the distribution.")
    env = dict(os.environ, ENACT_AGENT_BUILD="1", CARGO_TARGET_DIR=str(ROOT / "target"))
    args = ["cargo", "build", "--locked", "--bin", "zeroclaw", "--features", "embedded-web"]
    if not debug:
        args.append("--release")
    run(*args, cwd=SOURCE, env=env)
    binary = ROOT / "target" / ("debug" if debug else "release") / "zeroclaw"
    bindir = OUT / "bin"
    bindir.mkdir(exist_ok=True)
    # Replace the inode atomically. Overwriting a previously executed Mach-O
    # in place leaves macOS's cached code signature stale and causes SIGKILL.
    staged_binary = bindir / ".zeroclaw-new"
    shutil.copy2(binary, staged_binary)
    staged_binary.replace(bindir / "zeroclaw")
    shutil.copy2(ROOT / "overlays/enact/enact-agent", bindir / brand["command"])
    shutil.copytree(SOURCE / "web/dist", bindir / "web/dist", dirs_exist_ok=True)
    run(str(bindir / brand["command"]), "--version")


def native_build_environment(cargo, rustc, provenance):
    """Use one fixed encoded flag; spaces in HOME remain inside that flag."""
    home = os.environ["HOME"]
    if not Path(home).is_absolute() or home == Path(home).anchor or "\x1f" in home:
        raise ValueError("Native build requires an absolute non-root HOME without flag separators")
    return {"PATH": os.pathsep.join(dict.fromkeys([str(Path(cargo).parent), str(Path(rustc).parent),
                                                  "/usr/bin", "/bin", "/usr/sbin", "/sbin"])),
            "HOME": home, "LANG": "en_US.UTF-8", "ENACT_AGENT_BUILD": "1",
            "ENACT_AGENT_BUILD_ID": provenance["forkCommit"] + ("-dirty" if provenance["forkWorkingTreeDirty"] else ""),
            "CARGO_TARGET_DIR": str(OUT / "cargo-native"), "RUSTC": rustc,
            "CARGO_ENCODED_RUSTFLAGS": f"--remap-path-prefix={home}={PUBLIC_BUILD_HOME}"}


def require_private_home_absent(binary, home):
    try:
        canonical_home = Path(home).resolve()
    except (OSError, RuntimeError):
        raise RuntimeError("Cannot establish private build-home path guard; publication refused") from None
    contents = binary.read_bytes()
    if any(path in contents for path in {os.fsencode(home), os.fsencode(canonical_home)}):
        raise RuntimeError("Release binary contains the private build-home path; publication refused")


def native(brand, cargo, rustc, output_dir, committed=False, expected_commit=None):
    """Build a headless release with native local RPC, without web/channels."""
    source_digest = source_fingerprint(SOURCE)
    provenance = require_committed_release() if committed else source_provenance()
    if expected_commit is not None and provenance["forkCommit"] != expected_commit:
        raise RuntimeError("Release source commit changed before the build")
    release = release_manifest()
    target = next(line.removeprefix("host: ") for line in
                  subprocess.check_output([rustc, "-vV"], text=True).splitlines()
                  if line.startswith("host: "))
    destination = output_dir if output_dir else OUT / "native" / target
    destination.mkdir(parents=True, exist_ok=True)
    if committed and not (target.endswith("-apple-darwin") or "-linux-" in target):
        raise RuntimeError(f"Headless distribution releases currently require native macOS or Linux, got {target}")
    # Keep provider credentials and arbitrary compiler overrides out of Cargo.
    env = native_build_environment(cargo, rustc, provenance)
    run(cargo, "build", "--locked", *(["--offline"] if committed else []), "--release", "--no-default-features",
        "--features", "agent-runtime", "--target", target, "--bin", "zeroclaw", cwd=SOURCE, env=env)
    binary = destination / brand["command"]
    staged = destination / ".enact-agent-new"
    compiled = OUT / "cargo-native" / target / "release/zeroclaw"
    if committed:
        require_private_home_absent(compiled, env["HOME"])
    shutil.copy2(compiled, staged)
    staged.replace(binary)
    version_output = subprocess.check_output([str(binary), "--version"], text=True).strip()
    version = version_output.split()[1]
    if release["version"].split("-enact.")[0] != version:
        raise RuntimeError("Distribution release version must match the upstream binary version")
    if source_fingerprint(SOURCE) != source_digest:
        raise RuntimeError("Prepared source changed during the build")
    if committed and require_committed_release() != provenance:
        raise RuntimeError("Committed release source changed during the build")
    metadata = {
        "schemaVersion": 1,
        "binary": {"path": brand["command"], "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                   "sizeBytes": binary.stat().st_size, "target": target, "version": version},
        "protocol": {"name": f"{brand['command']}-local-rpc", "version": "1", "transport": "unix-ndjson"},
        "source": {"upstreamCommit": provenance["upstreamCommit"],
                   "overlay": {"kind": "local-snapshot", "sha256": source_digest}},
        "build": {"profile": "release"},
    }
    for filename in ("LICENSE-APACHE", "LICENSE-MIT", "NOTICE"):
        license_file = SOURCE / filename
        if not license_file.is_file():
            raise RuntimeError(f"Missing upstream attribution file: {license_file}")
        shutil.copy2(license_file, destination / filename)
    (destination / "UPSTREAM.txt").write_text(
        f"{brand['name']} is a branded distribution of ZeroClaw.\n"
        "Upstream: https://github.com/zeroclaw-labs/zeroclaw\n"
        f"Upstream source base: {provenance['upstreamCommit']}\n"
        f"Fork source commit: {provenance['forkCommit']}\n"
        f"Prepared source SHA-256: {source_digest}\n"
        "Internal compatibility identifiers and upstream licenses are retained.\n"
    )
    (destination / "manifest.json").write_text(json.dumps(metadata, indent=2) + "\n")
    (destination / "RELEASE.json").write_text(json.dumps({
        "schemaVersion": 1,
        "distribution": {"version": release["version"], "tag": f"{brand['command']}-v{release['version']}"},
        "source": provenance,
        "manifestSha256": hashlib.sha256((destination / "manifest.json").read_bytes()).hexdigest(),
        "build": {"defaultFeatures": False, "features": ["agent-runtime"],
                  "pathRemapping": {"from": "$HOME", "to": PUBLIC_BUILD_HOME,
                                    "privateHomeScan": "passed" if committed else "not-run"}},
    }, indent=2) + "\n")
    print(f"Built headless native artifact: {destination}")
    return metadata


def public_tar_member(member):
    """Exclude builder account identity from published archive metadata."""
    member.uid = member.gid = 0
    member.uname = member.gname = ""
    for key in ("uid", "gid", "uname", "gname", "SCHILY.uid", "SCHILY.gid",
                "SCHILY.uname", "SCHILY.gname"):
        member.pax_headers.pop(key, None)
    return member


def package_native_release(brand, artifact, output):
    """Create the single supported release archive and its checksum file."""
    metadata = json.loads((artifact / "manifest.json").read_text())
    release = json.loads((artifact / "RELEASE.json").read_text())
    if hashlib.sha256((artifact / "manifest.json").read_bytes()).hexdigest() != release["manifestSha256"]:
        raise RuntimeError("Native manifest does not match release provenance")
    binary = artifact / brand["command"]
    if (hashlib.sha256(binary.read_bytes()).hexdigest() != metadata["binary"]["sha256"]
            or binary.stat().st_size != metadata["binary"]["sizeBytes"]):
        raise RuntimeError("Native release binary does not match its manifest")
    for filename in ("LICENSE-APACHE", "LICENSE-MIT", "NOTICE"):
        if (artifact / filename).read_bytes() != (SOURCE / filename).read_bytes():
            raise RuntimeError(f"Release attribution changed: {filename}")
    (artifact / "branding").mkdir()
    for filename in (brand["logo"], brand["logoDark"]):
        shutil.copy2(SOURCE / "web/public" / filename, artifact / "branding" / filename)
    version = release["distribution"]["version"]
    stem = f"{release['distribution']['tag']}-{metadata['binary']['target']}"
    (artifact / "INSTALL.txt").write_text(
        f"{brand['name']} v{version} — {metadata['binary']['target']}\n\n"
        "This local release has no notarization or GitHub build attestation.\n"
        "SHA-256 checksums detect changes; they do not establish publisher identity.\n\n"
        f"After extracting the archive, enter {stem} and run:\n"
        "  shasum -a 256 -c SHA256SUMS    # macOS\n"
        "  sha256sum -c SHA256SUMS        # Linux\n  ./enact-agent --version\n\n"
        "Install the complete directory to preserve licenses and attribution:\n"
        "  mkdir -p \"$HOME/.local/share/enact-agent\" \"$HOME/.local/bin\"\n"
        f"  cp -R . \"$HOME/.local/share/enact-agent/{version}\"\n"
        f"  ln -sfn \"$HOME/.local/share/enact-agent/{version}/enact-agent\" \"$HOME/.local/bin/enact-agent\"\n"
        "  \"$HOME/.local/bin/enact-agent\" --help\n\n"
        "Run enact-agent quickstart to configure a provider, then enact-agent daemon.\n"
        "Native overrides use ENACT_AGENT_CONFIG_DIR, ENACT_AGENT_SOCKET and\n"
        "ENACT_AGENT_<lowercase_schema_path>, with __ replacing dots.\n"
        "Provider credentials belong in your private configuration, never this archive.\n"
    )
    files = sorted(path for path in artifact.rglob("*") if path.is_file())
    (artifact / "SHA256SUMS").write_text("".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(artifact).as_posix()}\n"
        for path in files))
    archive = output / f"{stem}.tar.gz"
    with tarfile.open(archive, "w:gz") as stream:
        stream.add(artifact, arcname=stem, filter=public_tar_member)
    (output / "SHA256SUMS").write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
    print(f"Packaged headless release: {archive}")
    return archive


def package(brand):
    package_dir = OUT / brand["command"]
    if package_dir.exists():
        shutil.rmtree(package_dir)
    (package_dir / "bin").mkdir(parents=True)
    shutil.copytree(OUT / "bin", package_dir / "bin", dirs_exist_ok=True)
    for license_file in [*ROOT.glob("LICENSE*"), ROOT / "NOTICE"]:
        if license_file.is_file():
            shutil.copy2(license_file, package_dir / license_file.name)
    shutil.copy2(ROOT / "overlays/enact/brand.json", package_dir / "brand.json")
    (package_dir / "UPSTREAM.txt").write_text(
        f"{brand['name']} is a branded distribution of ZeroClaw.\n"
        "Upstream: https://github.com/zeroclaw-labs/zeroclaw\n"
        "Internal compatibility identifiers and upstream licenses are retained.\n"
    )
    archive = OUT / f"{brand['command']}-{os.uname().sysname.lower()}-{os.uname().machine}.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(package_dir, arcname=brand["command"], filter=public_tar_member)
    print(f"Packaged: {archive}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["prepare", "build", "package", "desktop", "native", "release"])
    parser.add_argument("--debug", action="store_true", help="Build without release optimization")
    parser.add_argument("--cargo", default=shutil.which("cargo"), help="Cargo executable for native builds")
    parser.add_argument("--rustc", default=shutil.which("rustc"), help="Rust compiler for native builds")
    parser.add_argument("--output-dir", type=Path, help="Absolute native artifact directory")
    args = parser.parse_args()
    if args.action in {"native", "release"}:
        for name in ("cargo", "rustc"):
            executable = getattr(args, name)
            if not executable or not Path(executable).is_absolute() or not os.access(executable, os.X_OK):
                parser.error(f"--{name} requires an absolute executable path")
        if args.output_dir and not args.output_dir.is_absolute():
            parser.error("--output-dir must be absolute")
    if args.action in {"native", "release"} and args.debug:
        parser.error("native artifacts require the release profile")
    if args.action == "release":
        provenance = require_committed_release()
        release = release_manifest()
        destination = args.output_dir or OUT / "releases" / f"{manifest()['command']}-v{release['version']}"
        if destination.exists():
            parser.error("Release output directory must be new")
        brand = prepare(args.cargo, committed=True, revision=provenance["forkCommit"])
        destination.mkdir(parents=True)
        artifact = destination / brand["command"]
        native(brand, args.cargo, args.rustc, artifact, committed=True,
               expected_commit=provenance["forkCommit"])
        package_native_release(brand, artifact, destination)
        return
    brand = prepare(args.cargo or "cargo")
    if args.action == "native":
        native(brand, args.cargo, args.rustc, args.output_dir)
    if args.action in {"build", "package", "desktop"}:
        build(brand, args.debug)
    if args.action == "package":
        package(brand)
    if args.action == "desktop":
        desktop_icons(brand)
        env = dict(os.environ, ENACT_AGENT_BUILD="1", CARGO_TARGET_DIR=str(ROOT / "target"))
        triple = next(line.removeprefix("host: ") for line in
                      subprocess.check_output(["rustc", "-vV"], text=True).splitlines()
                      if line.startswith("host: "))
        sidecars = SOURCE / "apps/tauri/binaries"
        sidecars.mkdir(exist_ok=True)
        shutil.copy2(OUT / "bin/zeroclaw", sidecars / f"zeroclaw-{triple}")
        # Pin the local CLI; npm exec does not install a global tool.
        run("npm", "exec", "--yes", "--package=@tauri-apps/cli@2.12.3", "--", "tauri", "build",
            "--config", "tauri.bundled.conf.json", *(["--debug"] if args.debug else []),
            cwd=SOURCE / "apps/tauri", env=env)


if __name__ == "__main__":
    main()
