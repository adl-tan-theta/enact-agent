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
# Always + External only. OptIn SaaS/coding-cli adapters stay out of the ship set for now.
NATIVE_FEATURES = ("agent-runtime", "tools-external")
DISTRIBUTION_FEATURES = (*NATIVE_FEATURES, "embedded-web")


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


def remove_required(path, old):
    """Remove an anchored block that must be there exactly once."""
    text = path.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f"Upstream overlay drift: expected exactly one {old!r} in {path}")
    path.write_text(text.replace(old, ""))


def _family_table(families_rs):
    """The member table the overlay's family module declares.

    The table decides which operations a family advertises and whether a
    member takes its operation as an `action` argument, so it is checked
    against the operations inventory and against the tools themselves.
    """
    text = families_rs.read_text()
    members = {}
    pattern = re.compile(
        r"Member\s*\{\s*tool:\s*\"([^\"]+)\",\s*ops:\s*&\[(.*?)\],\s*"
        r"action_backed:\s*(true|false)\s*,?\s*\}",
        re.S,
    )
    for match in pattern.finditer(text):
        ops = re.findall(r'"([^"]+)"', match.group(2))
        members[match.group(1)] = (ops, match.group(3) == "true")
    families = {}
    for match in re.finditer(
        r'name:\s*"([^"]+)",\s*\n\s*description:\s*"[^"]*",\s*\n\s*members:\s*&\[(.*?)\]',
        text,
        re.S,
    ):
        families[match.group(1)] = re.findall(r'"([^"]+)"', match.group(2))
    return families, members


def _tool_declares_action(tool_source):
    """Whether a tool's parameter schema declares an `action` property."""
    text = tool_source.read_text()
    match = re.search(r"fn parameters_schema\([^)]*\)\s*->\s*[^{]*\{", text)
    if not match:
        return False
    body = text[match.end():match.end() + 20000]
    return bool(re.search(r'"action"\s*:\s*\{', body))


def check_enact_family_members(source):
    """Fail closed if the family table and the tools it describes disagree.

    The family table is the one place that says which operations a
    consolidated tool advertises. If it drifts from the operations
    inventory, the native surface and the canonical registry the bridge
    publishes would offer different actions under the same name — so the
    drift is a build failure, not a runtime surprise.
    """
    families_rs = ROOT / "overlays/enact/native/tools/families.rs"
    families, members = _family_table(families_rs)
    if not families or not members:
        raise RuntimeError("Upstream overlay drift: the family module declares no families")

    # Every member belongs to exactly one family, and every declared member
    # belongs to one at all.
    claimed = {}
    for family, names in families.items():
        for name in names:
            if name in claimed:
                raise RuntimeError(
                    f"Upstream overlay drift: {name} is claimed by both "
                    f"{claimed[name]} and {family}"
                )
            claimed[name] = family
    if set(claimed) != set(members):
        raise RuntimeError(
            "Upstream overlay drift: the family table and the member table disagree; "
            f"only in the families: {sorted(set(claimed) - set(members))}; "
            f"only in the members: {sorted(set(members) - set(claimed))}"
        )

    operations_csv = ROOT / "overlays/enact/zeroclaw-tools-operations.csv"
    ops_by_tool = {}
    source_by_tool = {}
    for line in operations_csv.read_text().splitlines()[1:]:
        if not line.strip():
            continue
        parts = line.split(",")
        tool = parts[0].lstrip("#")
        ops_by_tool.setdefault(tool, [])
        if parts[1] not in ops_by_tool[tool]:
            ops_by_tool[tool].append(parts[1])
        source_by_tool.setdefault(tool, parts[4])

    for tool, (ops, action_backed) in sorted(members.items()):
        csv_ops = ops_by_tool.get(tool)
        if csv_ops is None:
            raise RuntimeError(
                f"Upstream overlay drift: {tool} is a family member but is not "
                "in the operations inventory"
            )
        if csv_ops != ops:
            raise RuntimeError(
                f"Upstream overlay drift: the family table lists {ops} for {tool} "
                f"but the operations inventory lists {csv_ops}"
            )
        tool_source = source / source_by_tool[tool]
        if not tool_source.is_file():
            raise RuntimeError(f"Upstream overlay drift: the source of {tool} is missing")
        declares_action = _tool_declares_action(tool_source)
        if declares_action != action_backed:
            raise RuntimeError(
                f"Upstream overlay drift: {tool} "
                f"{'takes' if declares_action else 'does not take'} an action argument, "
                "but the family table says otherwise"
            )
        if declares_action:
            text = tool_source.read_text()
            missing = [op for op in ops if f'"{op}"' not in text]
            if missing:
                raise RuntimeError(
                    f"Upstream overlay drift: {tool} does not offer the operations {missing}"
                )


def enact_canonical_tool_names(csv_path=None, families_path=None):
    """The canonical tool names the build must advertise.

    Derived, never written down: every tool the operations inventory lists,
    plus every canonical family tool the consolidation adds. The bridge
    registry and the native MCP surface are generated from the same two
    sources, so this set is the single answer to "what does Enact ship?".
    """
    csv_path = csv_path or (ROOT / "overlays/enact/zeroclaw-tools-operations.csv")
    families_path = families_path or (ROOT / "overlays/enact/native/tools/families.rs")
    listed = set()
    for line in csv_path.read_text().splitlines()[1:]:
        if not line.strip():
            continue
        listed.add(line.split(",", 1)[0].lstrip("#"))
    families = set(re.findall(r'^        name: "([^"]+)",$', families_path.read_text(), flags=re.MULTILINE))
    if not families:
        raise RuntimeError("Upstream overlay drift: no canonical family tools found in the overlay")
    return listed | families


def check_enact_tool_inventory(source, csv_path=None, families_path=None):
    """Fail closed if the built-in inventory and the canonical tool set disagree.

    This runs against the prepared tree, after every projection has added its
    rows, so upstream drift, a stale operations inventory and a missing family
    row all fail the build instead of shipping a registry that disagrees with
    the canonical registry the bridge publishes.
    """
    inventory = source / "crates/zeroclaw-tools/src/inventory.rs"
    names = re.findall(r'^\s+name: "([^"]+)",$', inventory.read_text(), flags=re.MULTILINE)
    if len(set(names)) != len(names):
        raise RuntimeError(
            f"Enact tool inventory drift: the built-in inventory repeats a tool name "
            f"({len(names)} rows, {len(set(names))} names)"
        )
    expected = enact_canonical_tool_names(csv_path, families_path)
    registered = set(names)
    if registered != expected:
        missing = sorted(expected - registered)
        extra = sorted(registered - expected)
        raise RuntimeError(
            "Enact tool inventory drift: the built-in inventory and the canonical "
            f"tool set disagree; missing from the inventory: {missing}; "
            f"registered but not listed: {extra}"
        )


def install_enact_hook_wit(source):
    """Install the hook interface into the prepared tree.

    The package root and the crate-local copy are both needed: the host binds
    the crate-local one, and a fixture guest binds the package one.

    In a committed tree `crates/zeroclaw-plugins/wit` is a symlink to the
    package root, so the two names are one file and writing it twice is not
    drift. A working-tree preparation dereferences the symlink into a real
    directory, so the two names are two files and both are written. An
    interface that already exists with different content is drift.
    """
    overlay_wit = ROOT / "overlays/enact/native/plugins/hook.wit"
    payload = overlay_wit.read_bytes()
    installed = set()
    for relative in ("wit/v0/hook.wit", "crates/zeroclaw-plugins/wit/v0/hook.wit"):
        target_wit = source / relative
        key = os.path.realpath(target_wit)
        if key in installed:
            # The crate-local name resolves to the package root: one file.
            continue
        installed.add(key)
        if target_wit.exists():
            if target_wit.read_bytes() != payload:
                raise RuntimeError(
                    f"Upstream overlay drift: the hook interface at {target_wit} "
                    "differs from the overlay's"
                )
            continue
        target_wit.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(overlay_wit, target_wit)


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


def project_personality_templates(source):
    """Apply focused Enact identity edits while preserving upstream template structure."""
    template_dir = source / "crates/zeroclaw-runtime/src/agent/personality_templates"
    templates = {
        "IDENTITY.md": (
            "e65a35b94cd65887962e595db93fa3dbfd4ca63d1223ea422648426366c567ba",
            (
                ("# IDENTITY.md — Who Am I?", "# IDENTITY.md — Enact Agent"),
                ("- **Creature:** A Rust-forged AI — fast, lean, and relentless\n- **Vibe:** Sharp, direct, resourceful. Not corporate. Not a chatbot.\n- **Emoji:** 🦀",
                 "- **Product:** Enact OS\n- **Role:** An AI teammate working with people in their shared workspace.\n- **Vibe:** Clear, collaborative, capable, and direct."),
            ),
        ),
        "SOUL.md": (
            "2eb10b6abc062c75e1ac3466b9e97298473babc85fb439f4d37c47187f6d8f36",
            (
                ("# SOUL.md — Who You Are", "# SOUL.md — Enact Agent"),
                ("*You're not a chatbot. You're becoming someone.*", "*You're an AI teammate in Enact OS, working with people in their shared workspace.*"),
                ("You are **{agent}**. Built in Rust. 3MB binary. Zero bloat.\nYou are NOT ChatGPT, Claude, DeepSeek, Gemini, or any other product.\nYou are {agent}. That is your name. That is who you are.\n\n- NEVER say \"As an AI\" or \"I'm just an AI\"\n- NEVER mention OpenAI, Anthropic, DeepSeek, Google by name\n- Always introduce yourself as {agent} if asked",
                 "You are **{agent}**, an AI teammate in Enact OS. Your active profile, model, tools, and permissions depend on Enact's current configuration.\n\n- Always introduce yourself as {agent} if asked\n- Do not claim access to tools or workspace context unless the active session provides it\n- If asked about your model or provider, report only what the active runtime exposes"),
                ("Each session, you wake up fresh. These files ARE your memory.\nRead them. Update them. They're how you persist.",
                 "These agent workspace files provide local continuity between sessions. They are separate from Enact's workspace Memory Graph; use that graph only through tools exposed to the active session."),
            ),
        ),
        "USER.md": (
            "435a84e5ed0d1f1257b428ddf35828be894a7f2f15a99c2fa5324c3a0daaa2e3",
            (),
        ),
        "AGENTS.md": (
            "51aed384f8ab2d046b00464bae4ba077e762fc314447ec091165bebea28716a8",
            (
                ("# AGENTS.md — {agent} Personal Assistant", "# AGENTS.md — {agent} in Enact OS"),
                ("4. If in MAIN SESSION (direct chat): `MEMORY.md` is already injected",
                 "4. If in the main Enact session, use `MEMORY.md` only when the active runtime injects it"),
                ("- **Long-term:** `MEMORY.md` — curated memories (auto-injected in main session)",
                 "- **Long-term:** `MEMORY.md` — curated local memories, when included by the active runtime"),
                ("Skip secrets unless asked to keep them.", "Never store secrets, even when asked."),
            ),
        ),
        "AGENTS.no-memory.md": (
            "43277aa219dd317e7059ed03e06f19f03948eb41d4eef837bea1ba34f1643c6f",
            (
                ("# AGENTS.md — {agent} Personal Assistant", "# AGENTS.md — {agent} in Enact OS"),
            ),
        ),
        "TOOLS.md": (
            "aab5f5b85288823a832262e1d16d29e8833044b1263857a5c04b19eef0bc4696",
            (
                ("# TOOLS.md — Local Notes", "# TOOLS.md — Enact Agent local notes"),
                ("## Built-in Tools\n", "## Built-in Tools\n\nThe tools below may be provided by this runtime; actual availability depends on the active Enact session and its permissions.\n\n"),
            ),
        ),
        "HEARTBEAT.md": (
            "f525101617a48bbdabea2625c25e9d3face0cdb48655fb2b94d1c70a0647804e",
            (
                ("# HEARTBEAT.md", "# HEARTBEAT.md — Enact Agent daemon"),
                ("# Keep this file empty (or with only comments) to skip heartbeat work.\n# Add tasks below when you want {agent} to check something periodically.",
                 "# The Enact Agent daemon reads '- ' task lines when heartbeat is enabled.\n# A configured heartbeat.message can still run when this file is empty.\n# Base cadence is configured with heartbeat.interval_minutes; runtime may adjust it.\n# Add tasks below when you want {agent} to check something periodically."),
            ),
        ),
        "MEMORY.md": (
            "042e86b07f9491642574f1736995d96a8334caf058007d1fc5ec34334e4860a7",
            (
                ("# MEMORY.md — Long-Term Memory", "# MEMORY.md — Long-Term Memory for Enact Agent"),
                ("*Your curated memories. The distilled essence, not raw logs.*",
                 "*Curated local memories for this Enact Agent. The distilled essence, not raw logs.*"),
                ("- This file is auto-injected into your system prompt each session",
                 "- The active runtime determines whether this file is injected into a session"),
                ("- ONLY loaded in main session (direct chat with your human)\n- NEVER loaded in group chats or shared contexts",
                 "- Never store secrets.\n- Session injection depends on active runtime and channel policy. Keep private notes out of shared replies."),
                ("## How This Works\n", "## How This Works\n\nThis local file is separate from Enact's workspace Memory Graph. Use the graph only through tools exposed to the active session.\n\n"),
            ),
        ),
    }
    for filename, (expected_sha256, edits) in templates.items():
        path = template_dir / filename
        source_bytes = path.read_bytes()
        actual_sha256 = hashlib.sha256(source_bytes).hexdigest()
        if actual_sha256 != expected_sha256:
            raise RuntimeError(
                f"Upstream overlay drift: review Enact persona projection for {path} "
                f"(expected {expected_sha256}, got {actual_sha256})"
            )
        text = source_bytes.decode("utf-8")
        for old, new in edits:
            if text.count(old) != 1:
                raise RuntimeError(f"Upstream overlay drift: expected exactly one {old!r} in {path}")
            text = text.replace(old, new)
        path.write_text(text)


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


def project_workspace_owned_agent_paths(source):
    """Keep standalone agent state under Enact home, with workspace-first ownership."""
    schema = source / "crates/zeroclaw-config/src/schema.rs"
    replace_required(
        schema,
        '''    /// Returns the agent's `[agents.<alias>.workspace.path]` override
    /// when set (operator-explicit, e.g. for putting a workspace on a
    /// different disk), otherwise derives
    /// `<install>/agents/<alias>/workspace/` from the install root
    /// (the directory containing `config.toml`).
    ///
    /// Per-agent workspaces live under
    /// `<install>/agents/<alias>/workspace/` and hold the agent's
    /// markdown memory (MEMORY.md), identity files (IDENTITY.md,
    /// SOUL.md), and any other per-agent plaintext state. Shared
    /// databases (SQLite memory, sessions, cost records) live under
    /// `config.data_dir` instead and partition by agent at the row
    /// level. Per-agent overrides via `[agents.<alias>.workspace.path]`
    /// pin an arbitrary filesystem path (e.g. a different mount).''',
        '''    /// Returns the agent's `[agents.<alias>.workspace.path]` override
    /// when set; otherwise resolves the default workspace below
    /// `$ENACT_HOME/workspace/agents/<alias>`.
    ///
    /// This is the standalone Enact Agent default. Enact-hosted sessions
    /// receive an explicit private workspace projection and therefore do
    /// not use this default as their durable profile store. Shared runtime
    /// databases remain under `config.data_dir`.''',
    )
    replace_required(
        schema,
        '''        self.install_root_dir()
            .join("agents")
            .join(agent_alias)
            .join("workspace")''',
        '''        self.install_root_dir()
            .join("workspace")
            .join("agents")
            .join(agent_alias)''',
    )
    replace_required(
        schema,
        '''    /// Install root: `<install>/` derived from `config_path`'s parent. Used
    /// to compute `<install>/shared/`, `<install>/agents/`, and the
    /// skill-bundle directory defaults. Public so consumers (gateway, CLI,
    /// SkillsService) share the same anchor.
    #[must_use]
    pub fn install_root_dir(&self) -> std::path::PathBuf {
        self.config_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    }''',
        '''    /// Enact Agent state root. The default config lives in `agent/`, while
    /// workspace-owned content lives beside it under `workspace/`. An
    /// explicitly selected config directory remains its own root.
    #[must_use]
    pub fn install_root_dir(&self) -> std::path::PathBuf {
        let config_dir = self.config_path.parent().map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        if std::env::var_os("ENACT_AGENT_CONFIG_DIR").is_some() {
            return config_dir;
        }
        if let Some(enact_home) = std::env::var_os("ENACT_HOME")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            && config_dir == enact_home.join("agent")
        {
            return enact_home;
        }
        if config_dir.file_name().is_some_and(|name| name == "agent")
            && config_dir.parent().is_some_and(|parent| parent.file_name().is_some_and(|name| name == ".enact"))
        {
            return config_dir.parent().map(std::path::Path::to_path_buf)
                .unwrap_or(config_dir);
        }
        config_dir
    }''',
    )
    replace_required(
        schema,
        '''/// Resolution order: `ZEROCLAW_CONFIG_DIR` env → `ZEROCLAW_WORKSPACE` env → `~/.zeroclaw/config.toml`.''',
        '''/// Resolution order: `ENACT_AGENT_CONFIG_DIR` env → `ZEROCLAW_WORKSPACE` env → `$ENACT_HOME/agent/config.toml`.''',
    )
    replace_required(
        schema,
        '''    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home).join(".zeroclaw"));
    }

    let home = UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    Ok(home.join(".zeroclaw"))''',
        '''    if let Ok(home) = std::env::var("ENACT_HOME")
        && !home.trim().is_empty()
    {
        return Ok(PathBuf::from(home).join("agent"));
    }

    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home).join(".enact").join("agent"));
    }

    let home = UserDirs::new()
        .map(|u| u.home_dir().join(".enact").join("agent"))
        .context("Could not find home directory")?;
    Ok(home)''',
    )
    replace_required(
        schema,
        '''/// `<config_dir>` honors `ZEROCLAW_CONFIG_DIR` and otherwise defaults to
/// `~/.zeroclaw`. The zerocode binary mirrors this path inline (it carries no
/// `zeroclaw-*` dependency).''',
        '''/// `<config_dir>` honors `ENACT_AGENT_CONFIG_DIR` and otherwise defaults to
/// `$ENACT_HOME/agent`. The zerocode binary mirrors this path inline.''',
    )
    native = source / "crates/zeroclaw-config/src/platform/native.rs"
    replace_required(
        native,
        '''        directories::UserDirs::new().map_or_else(
            || PathBuf::from(".zeroclaw"),
            |u| u.home_dir().join(".zeroclaw"),
        )''',
        '''        std::env::var_os("ENACT_HOME")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().join(".enact")))
            .unwrap_or_else(|| PathBuf::from(".enact"))
            .join("agent")''',
    )
    bundle = source / "crates/zeroclaw-config/src/agent_bundle.rs"
    replace_required(bundle, 'default location (`agents/<alias>/workspace`)',
                     'default location (`workspace/agents/<alias>`)')
    exporter = source / "src/alias_cli/export.rs"
    replace_required(exporter, 'const WORKSPACE_FAMILY: &str = "agents";',
                     'const WORKSPACE_FAMILY: &str = "workspace";')
    exporter_text = exporter.read_text()
    for old, new in (
        ('agents tree', 'workspace tree'),
        ('agents/researcher/workspace', 'workspace/agents/researcher'),
    ):
        if old not in exporter_text:
            raise RuntimeError(f"Upstream overlay drift: expected {old!r} in {exporter}")
        exporter_text = exporter_text.replace(old, new)
    exporter.write_text(exporter_text)
    replace_required(
        source / "crates/zeroclaw-config/src/agent_bundle.rs",
        '"agents/researcher/workspace"',
        '"workspace/agents/researcher"',
    )


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
        let grants = self.recheck_authority_after_admission(Method::SessionTools)?;
        let scope = self.scoped_principal_id_from(grants.as_ref());
        self.revalidate_admitted_session(
            &req.session_id,
            authorized.as_ref(),
            scope.as_deref(),
        )
        .await?;
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


def project_enact_builtin_docs_skill(source):
    """Ship Enact-facing instructions for the embedded product-docs skill."""
    asset = source / "crates/zeroclaw-runtime/src/skills/assets/zeroclaw-docs/SKILL.md"
    original = asset.read_bytes()
    expected_sha256 = "435f3a50ef1561e7945f7f382fcb9ef1cbe9350d4e17161d955837301387921d"
    if hashlib.sha256(original).hexdigest() != expected_sha256:
        raise RuntimeError(f"Upstream overlay drift: review built-in docs skill at {asset}")

    overlay = ROOT / "overlays/enact/skills/enact-agent-docs/SKILL.md"
    content = overlay.read_text()
    if not content.startswith("---\nname: enact-agent-docs\n"):
        raise RuntimeError(f"Enact built-in docs skill has invalid frontmatter: {overlay}")
    asset.write_text(content)


def project_enact_agent_capabilities(source):
    """Fail closed if Always/External ship capabilities or inventory anchors drift."""
    inventory = source / "crates/zeroclaw-tools/src/inventory.rs"
    inventory_text = inventory.read_text()
    for name in ("spawn_subagent", "delegate"):
        if inventory_text.count(f'name: "{name}"') != 1:
            raise RuntimeError(f"Upstream overlay drift: expected one {name} inventory entry in {inventory}")
    inventory_names = re.findall(r'^\s+name: "([^"]+)",$', inventory_text, flags=re.MULTILINE)
    if len(set(inventory_names)) != len(inventory_names):
        raise RuntimeError(
            f"Upstream overlay drift: the built-in inventory repeats a tool name "
            f"({len(inventory_names)} rows, {len(set(inventory_names))} names)"
        )
    # The advertised tool count is DERIVED, never written down. It is compared
    # against the *prepared* tree, once every projection has added its rows:
    # `check_enact_tool_inventory` runs at the end of `prepare()`. Comparing
    # here instead would judge the tree against the canonical set before the
    # overlay's own rows exist, and blame upstream drift for the overlay's
    # own work.
    runtime_tools = source / "crates/zeroclaw-runtime/src/tools/mod.rs"
    runtime_text = runtime_tools.read_text()
    required_runtime_anchors = (
        "register_tool!(\"spawn_subagent\"",
        "SpawnSubagentTool::new(",
        "DelegateTool::new_with_options(",
        "tool_arcs.push(delegate_tool as Arc<dyn Tool>)",
    )
    for anchor in required_runtime_anchors:
        if runtime_text.count(anchor) != 1:
            raise RuntimeError(f"Upstream overlay drift: expected one {anchor!r} in {runtime_tools}")
    cargo = tomllib.loads((source / "Cargo.toml").read_text())
    features = cargo.get("features", {}).get("agent-runtime", [])
    if "dep:zeroclaw-tools" not in features:
        raise RuntimeError("Enact headless build must retain zeroclaw-tools in agent-runtime")
    if "tools-external" not in cargo.get("features", {}):
        raise RuntimeError("Enact headless build requires ZeroClaw's tools-external feature")
    feature_graph = cargo["features"]
    selected = set()

    def visit(feature):
        if feature in selected:
            return
        selected.add(feature)
        for dependency in feature_graph.get(feature, []):
            visit(dependency)

    for feature in NATIVE_FEATURES:
        visit(feature)
    if "tools-external" not in selected:
        raise RuntimeError("Enact headless build must select tools-external for External adapters")
    opt_in = {name for name in feature_graph if name.startswith("tool-")}
    unexpected = sorted(opt_in & selected)
    if unexpected:
        raise RuntimeError(
            f"Enact Always/External ship set must omit OptIn tool features: {unexpected}"
        )

    # Compile the capability contract against the same graph as the binary.
    # Keep the durable test overlay-owned; never replace an upstream test.
    target = source / "tests/enact_builtin_capabilities.rs"
    if target.exists():
        raise RuntimeError(f"Upstream overlay drift: Enact capability test already exists at {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source / "overlays/enact/tests/builtin_capabilities.rs", target)


def project_enact_mcp_server(source):
    """Ship the Enact MCP server so one registry serves Codex CLI and Claude Code.

    The module is overlay-authored: it lives under `overlays/enact/native/` and is
    copied into the prepared source, so no tracked `src/` or `crates/` Rust
    changes and no `Cargo.toml` / `Cargo.lock` churn. Every dependency it uses
    is already a dependency of the root package.
    """
    overlay = ROOT / "overlays/enact/native/mcp-server/src"
    target = source / "src/mcp_server"
    if target.exists():
        raise RuntimeError(f"Upstream overlay drift: MCP server module already exists at {target}")
    shutil.copytree(overlay, target)

    main_source = source / "src/main.rs"
    replace_required(
        main_source,
        '#[cfg(feature = "agent-runtime")]\nmod i18n;\n',
        '#[cfg(feature = "agent-runtime")]\nmod i18n;\n'
        '#[cfg(feature = "agent-runtime")]\nmod mcp_server;\n',
    )
    replace_required(
        main_source,
        "#[derive(Subcommand, Debug)]\nenum Commands {\n",
        "#[derive(Subcommand, Debug)]\nenum Commands {\n"
        "    /// Serve the built-in tool registry to MCP hosts (Codex CLI, Claude Code).\n"
        '    #[cfg(feature = "agent-runtime")]\n'
        "    Mcp {\n"
        "        #[command(subcommand)]\n"
        "        command: mcp_server::cli::McpCommands,\n"
        "    },\n\n",
    )
    replace_required(
        main_source,
        '    #[cfg(feature = "agent-runtime")]\n    match cli.command {\n        Commands::Onboard { .. }\n',
        '    #[cfg(feature = "agent-runtime")]\n    match cli.command {\n'
        "        Commands::Mcp { command } => {\n"
        "            mcp_server::cli::run(command).await?;\n"
        "            Ok(())\n"
        "        }\n\n"
        "        Commands::Onboard { .. }\n",
    )


def project_enact_declarative_hooks(source):
    """Ship declarative command hooks and post-call output rewriting.

    Two overlay-authored modules become the hook system:

    * `zeroclaw-config::hooks` — the declarative rule shape (`event`,
      `command`, `matcher`, `timeout_ms`, `fail_open`, `env`, `priority`),
      wired into `hooks.commands` in the config schema.
    * `zeroclaw-runtime::hooks::command` — the `HookHandler` that runs a
      configured command, feeds it the event payload as JSON on stdin, and
      reads a JSON decision (`continue` / `block` / `rewrite`) from stdout.

    The runtime also gains a *modifying* `after_tool_call` hook: the result
    is rewritten before it is logged and before it reaches the model, so a
    redaction hook keeps a secret out of both. Audit hooks still observe the
    original result.

    Both modules live under `overlays/enact/native/` and are copied into the
    prepared source, so no tracked `src/` or `crates/` Rust changes and no
    `Cargo.toml` / `Cargo.lock` churn: every dependency they use is already a
    dependency of the crate they land in.
    """
    overlay = ROOT / "overlays/enact/native/hooks"

    config_module = source / "crates/zeroclaw-config/src/hooks.rs"
    if config_module.exists():
        raise RuntimeError(f"Upstream overlay drift: hooks config module already exists at {config_module}")
    shutil.copy2(overlay / "config.rs", config_module)

    command_module = source / "crates/zeroclaw-runtime/src/hooks/command.rs"
    if command_module.exists():
        raise RuntimeError(f"Upstream overlay drift: command hook module already exists at {command_module}")
    command_module.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(overlay / "command.rs", command_module)

    # ── config: expose the declarative rule shape ────────────────────────
    replace_required(
        source / "crates/zeroclaw-config/src/lib.rs",
        "pub mod helpers;\n",
        "pub mod helpers;\npub mod hooks;\n",
    )

    replace_required(
        source / "crates/zeroclaw-config/src/schema.rs",
        "    #[serde(default)]\n"
        "    #[nested]\n"
        "    pub builtin: BuiltinHooksConfig,\n"
        "}",
        "    #[serde(default)]\n"
        "    #[nested]\n"
        "    pub builtin: BuiltinHooksConfig,\n"
        "    /// Declarative hook rules. Each entry maps a lifecycle event to a\n"
        "    /// command: the command receives the event payload as JSON on stdin\n"
        "    /// and answers with a JSON decision (`continue`, `block`, or\n"
        "    /// `rewrite`) on stdout. A script is a hook — no plugin runtime and\n"
        "    /// no rebuild required.\n"
        "    ///\n"
        "    /// ```toml\n"
        "    /// [[hooks.commands]]\n"
        "    /// event = \"before_tool_call\"\n"
        "    /// matcher = \"shell\"\n"
        "    /// command = \"bin/audit-shell.sh\"\n"
        "    /// timeout_ms = 2000\n"
        "    /// ```\n"
        "    #[serde(default)]\n"
        "    pub commands: Vec<crate::hooks::HookCommandConfig>,\n"
        "}",
    )

    replace_required(
        source / "crates/zeroclaw-config/src/schema.rs",
        "        Self {\n"
        "            enabled: true,\n"
        "            builtin: BuiltinHooksConfig::default(),\n"
        "        }",
        "        Self {\n"
        "            enabled: true,\n"
        "            builtin: BuiltinHooksConfig::default(),\n"
        "            commands: Vec::new(),\n"
        "        }",
    )

    # ── api: the modifying after_tool_call hook ──────────────────────────
    replace_required(
        source / "crates/zeroclaw-api/src/hook.rs",
        "    /// Inspect or modify a tool call with explicit correlation context.",
        "    /// Inspect or modify the result of a tool call.\n"
        "    ///\n"
        "    /// Runs sequentially by priority after the tool completed and before\n"
        "    /// the result is logged or handed to the model, so a handler that\n"
        "    /// rewrites the output keeps the original text out of both. A\n"
        "    /// handler that cancels the result stops it from reaching the model.\n"
        "    ///\n"
        "    /// Observing handlers (`on_after_tool_call`) still see the original\n"
        "    /// result: rewriting is for what the model reads, not for what an\n"
        "    /// auditor records.\n"
        "    async fn after_tool_call(&self, _tool: &str, _result: &ToolResult) -> HookResult<ToolResult> {\n"
        "        HookResult::Continue(_result.clone())\n"
        "    }\n"
        "\n"
        "    /// Inspect or modify a tool call with explicit correlation context.",
    )

    # ── runtime: register the configured commands ────────────────────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/hooks/mod.rs",
        "pub mod builtin;\n",
        "pub mod builtin;\npub mod command;\n",
    )

    # The upstream runner tests build `HooksConfig` literally; the new
    # declarative list is part of that struct, so those literals carry it.
    for builtin_block in (
        "                command_logger: false,\n"
        "                webhook_audit: zeroclaw_config::schema::WebhookAuditConfig::default(),\n"
        "            },\n",
        "                command_logger: true,\n"
        "                webhook_audit: zeroclaw_config::schema::WebhookAuditConfig::default(),\n"
        "            },\n",
        "                command_logger: true,\n"
        "                webhook_audit: zeroclaw_config::schema::WebhookAuditConfig {\n"
        "                    enabled: true,\n"
        "                    url: \"http://example.com/audit\".to_string(),\n"
        "                    ..Default::default()\n"
        "                },\n"
        "            },\n",
    ):
        replace_required(
            source / "crates/zeroclaw-runtime/src/hooks/runner.rs",
            builtin_block,
            builtin_block + "            commands: Vec::new(),\n",
        )

    replace_required(
        source / "crates/zeroclaw-runtime/src/hooks/runner.rs",
        "        runner\n    }\n\n    /// Register a handler and re-sort by descending priority.",
        "        for rule in &hooks.commands {\n"
        "            runner.register(Box::new(super::command::CommandHook::new(rule.clone())));\n"
        "        }\n"
        "        runner\n"
        "    }\n"
        "\n"
        "    /// Register a handler and re-sort by descending priority.",
    )

    replace_required(
        source / "crates/zeroclaw-runtime/src/hooks/runner.rs",
        "    pub async fn run_on_message_received(\n",
        "    /// Run the modifying after-call chain: sequential by priority,\n"
        "    /// short-circuit on `Cancel`, panic-isolated per handler.\n"
        "    ///\n"
        "    /// Each handler sees the result the previous handler produced, so a\n"
        "    /// redaction hook and a size-limiting hook compose in priority\n"
        "    /// order.\n"
        "    pub async fn run_after_tool_call(\n"
        "        &self,\n"
        "        tool: &str,\n"
        "        result: &ToolResult,\n"
        "    ) -> HookResult<ToolResult> {\n"
        "        let mut current = result.clone();\n"
        "        for h in &self.handlers {\n"
        "            let hook_name = h.name();\n"
        "            match AssertUnwindSafe(h.after_tool_call(tool, &current))\n"
        "                .catch_unwind()\n"
        "                .await\n"
        "            {\n"
        "                Ok(HookResult::Continue(next)) => current = next,\n"
        "                Ok(HookResult::Cancel(reason)) => {\n"
        "                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({\"hook\": hook_name, \"reason\": reason.to_string()})), \"after_tool_call cancelled by hook\");\n"
        "                    return HookResult::Cancel(reason);\n"
        "                }\n"
        "                Err(_) => {\n"
        "                    ::zeroclaw_log::record!(\n"
        "                        ERROR,\n"
        "                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)\n"
        "                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)\n"
        "                            .with_attrs(::serde_json::json!({\"hook\": hook_name})),\n"
        "                        \"after_tool_call hook panicked; continuing with the previous result\"\n"
        "                    );\n"
        "                }\n"
        "            }\n"
        "        }\n"
        "        HookResult::Continue(current)\n"
        "    }\n"
        "\n"
        "    pub async fn run_on_message_received(\n",
    )

    # ── runtime: apply the rewritten result before it is logged ───────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/post_exec.rs",
        "    for (((idx, call), stream_call), outcome) in executable_indices\n",
        "    for (((idx, call), stream_call), mut outcome) in executable_indices\n",
    )

    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/post_exec.rs",
        "        ::zeroclaw_log::record!(\n"
        "            INFO,\n"
        "            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)\n"
        "                .with_category(::zeroclaw_log::EventCategory::Tool)\n",
        "        // ── Hook: after_tool_call (modifying) ─────────────\n"
        "        // Runs before the result is logged and before it reaches the model,\n"
        "        // so a redaction hook keeps the original text out of both. The\n"
        "        // observing hook below still sees the original result.\n"
        "        let mut original_result: Option<crate::tools::ToolResult> = None;\n"
        "        if let Some(hooks) = ctx.hooks {\n"
        "            let original = crate::tools::ToolResult {\n"
        "                success: outcome.success,\n"
        "                output: outcome.output.clone().into(),\n"
        "                error: None,\n"
        "            };\n"
        "            match hooks.run_after_tool_call(&call.name, &original).await {\n"
        "                zeroclaw_api::hook::HookResult::Continue(rewritten) => {\n"
        "                    let rewritten_text = rewritten.output.as_str().to_string();\n"
        "                    if rewritten_text != outcome.output {\n"
        "                        outcome.output = rewritten_text;\n"
        "                    }\n"
        "                    if !rewritten.success {\n"
        "                        outcome.success = false;\n"
        "                    }\n"
        "                    if let Some(error) = rewritten.error.as_ref() {\n"
        "                        outcome.error_reason = Some(error.clone());\n"
        "                    }\n"
        "                }\n"
        "                zeroclaw_api::hook::HookResult::Cancel(reason) => {\n"
        "                    outcome.success = false;\n"
        "                    outcome.output = reason.clone();\n"
        "                    outcome.error_reason = Some(reason);\n"
        "                }\n"
        "            }\n"
        "            original_result = Some(original);\n"
        "        }\n"
        "\n"
        "        ::zeroclaw_log::record!(\n"
        "            INFO,\n"
        "            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)\n"
        "                .with_category(::zeroclaw_log::EventCategory::Tool)\n",
    )

    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/post_exec.rs",
        "        // ── Hook: after_tool_call (void) ─────────────────\n"
        "        if let Some(hooks) = ctx.hooks {\n"
        "            let hook_context = crate::hooks::tool_call_hook_context(ctx.turn_id, iteration, *idx);\n"
        "            let tool_result_obj = crate::tools::ToolResult {\n"
        "                success: outcome.success,\n"
        "                output: outcome.output.clone().into(),\n"
        "                error: None,\n"
        "            };\n"
        "            // The prepared arguments travel with the completion call so\n"
        "            // argument-auditing hooks export what was actually dispatched and\n"
        "            // never need to retain arguments between the hook phases.\n"
        "            hooks\n"
        "                .fire_after_tool_call_with_context_and_args(\n"
        "                    &hook_context,\n"
        "                    &call.name,\n"
        "                    &call.arguments,\n"
        "                    &tool_result_obj,\n"
        "                    outcome.duration,\n"
        "                )\n"
        "                .await;\n"
        "        }\n",
        "        // ── Hook: after_tool_call (void, observing) ───────\n"
        "        if let (Some(hooks), Some(tool_result_obj)) = (ctx.hooks, original_result.as_ref()) {\n"
        "            let hook_context = crate::hooks::tool_call_hook_context(ctx.turn_id, iteration, *idx);\n"
        "            // The prepared arguments travel with the completion call so\n"
        "            // argument-auditing hooks export what was actually dispatched and\n"
        "            // never need to retain arguments between the hook phases. The\n"
        "            // result is the original one: a rewriting hook changed what the\n"
        "            // model reads, not what an auditor records.\n"
        "            hooks\n"
        "                .fire_after_tool_call_with_context_and_args(\n"
        "                    &hook_context,\n"
        "                    &call.name,\n"
        "                    &call.arguments,\n"
        "                    tool_result_obj,\n"
        "                    outcome.duration,\n"
        "                )\n"
        "                .await;\n"
        "        }\n",
    )


def project_enact_trust_split(source):
    """Split the `before_llm_call` request into trusted and untrusted context.

    The hook path hands a handler the whole message vector, so a handler that
    wanted to sanitize or rewrite the externally-authored half had no way to
    tell it apart from the operator's own instructions. This adds:

    * `zeroclaw_api::hook::LlmCallHookContext` — the trust classification of
      each message, plus the trust class of the turn that produced the
      request;
    * `HookHandler::before_llm_call_with_context` — a context-aware variant
      that defaults to the existing `before_llm_call`, so every current
      handler keeps working unchanged;
    * `agent::turn::trust_split` — the runtime-side classifier, which marks
      tool output and externally-framed payloads untrusted, and user messages
      untrusted when the turn's own trust class says so.
    """
    overlay = ROOT / "overlays/enact/native/hooks/trust.rs"
    target = source / "crates/zeroclaw-runtime/src/hooks/trust.rs"
    if target.exists():
        raise RuntimeError(f"Upstream overlay drift: trust split module already exists at {target}")
    shutil.copy2(overlay, target)

    api_hook = source / "crates/zeroclaw-api/src/hook.rs"

    replace_required(
        api_hook,
        "use crate::channel::ChannelMessage;\n",
        "use crate::channel::ChannelMessage;\nuse crate::ingress::TrustClass;\n",
    )

    replace_required(
        api_hook,
        "/// Result of a modifying hook — continue with (possibly modified) data, or cancel.",
        "/// How one message of an LLM request is classified for trust.\n"
        "///\n"
        "/// The split matters because a handler that sanitizes, rewrites, or\n"
        "/// audits a request must be able to tell the operator's own\n"
        "/// instructions from text that arrived from somewhere else.\n"
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n"
        "pub enum MessageTrust {\n"
        "    /// Operator- or system-authored context: the system prompt, the\n"
        "    /// memory preamble, configured skills, the agent's own prior turns.\n"
        "    Trusted,\n"
        "    /// Externally-authored content: channel messages, tool output,\n"
        "    /// fetched documents, delegated agent output.\n"
        "    Untrusted,\n"
        "}\n"
        "\n"
        "/// The trust split of one LLM request.\n"
        "///\n"
        "/// `before_llm_call` hands a handler the whole message vector. This\n"
        "/// context says which messages carry externally-authored content, so a\n"
        "/// handler can act on the untrusted half alone and leave the\n"
        "/// instructions alone.\n"
        "#[derive(Debug, Clone)]\n"
        "pub struct LlmCallHookContext {\n"
        "    /// The trust class of the turn that produced the request.\n"
        "    pub turn_trust: TrustClass,\n"
        "    /// The trust of each message, parallel to the request's message\n"
        "    /// vector.\n"
        "    pub message_trust: Vec<MessageTrust>,\n"
        "}\n"
        "\n"
        "impl Default for LlmCallHookContext {\n"
        "    fn default() -> Self {\n"
        "        Self {\n"
        "            turn_trust: TrustClass::Trusted,\n"
        "            message_trust: Vec::new(),\n"
        "        }\n"
        "    }\n"
        "}\n"
        "\n"
        "impl LlmCallHookContext {\n"
        "    /// Build a context from a turn trust class and a per-message split.\n"
        "    #[must_use]\n"
        "    pub fn new(turn_trust: TrustClass, message_trust: Vec<MessageTrust>) -> Self {\n"
        "        Self {\n"
        "            turn_trust,\n"
        "            message_trust,\n"
        "        }\n"
        "    }\n"
        "\n"
        "    /// Whether the message at `index` carries untrusted content.\n"
        "    #[must_use]\n"
        "    pub fn is_untrusted(&self, index: usize) -> bool {\n"
        "        self.message_trust\n"
        "            .get(index)\n"
        "            .is_some_and(|trust| *trust == MessageTrust::Untrusted)\n"
        "    }\n"
        "\n"
        "    /// The indices of the messages that carry untrusted content.\n"
        "    #[must_use]\n"
        "    pub fn untrusted_indices(&self) -> Vec<usize> {\n"
        "        self.message_trust\n"
        "            .iter()\n"
        "            .enumerate()\n"
        "            .filter(|(_, trust)| **trust == MessageTrust::Untrusted)\n"
        "            .map(|(index, _)| index)\n"
        "            .collect()\n"
        "    }\n"
        "\n"
        "    /// The number of messages that carry untrusted content.\n"
        "    #[must_use]\n"
        "    pub fn untrusted_count(&self) -> usize {\n"
        "        self.message_trust\n"
        "            .iter()\n"
        "            .filter(|trust| **trust == MessageTrust::Untrusted)\n"
        "            .count()\n"
        "    }\n"
        "\n"
        "    /// The number of messages that carry trusted content.\n"
        "    #[must_use]\n"
        "    pub fn trusted_count(&self, total: usize) -> usize {\n"
        "        total.saturating_sub(self.untrusted_count())\n"
        "    }\n"
        "}\n"
        "\n"
        "/// Result of a modifying hook — continue with (possibly modified) data, or cancel.",
    )

    replace_required(
        api_hook,
        "    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {",
        "    /// Inspect or modify an LLM request with its trust split.\n"
        "    ///\n"
        "    /// The default delegates to [`HookHandler::before_llm_call`], so a\n"
        "    /// handler that does not care about trust keeps working unchanged. A\n"
        "    /// handler that does care can act on the untrusted messages alone,\n"
        "    /// without being able to rewrite the operator's instructions by\n"
        "    /// accident.\n"
        "    async fn before_llm_call_with_context(\n"
        "        &self,\n"
        "        _context: &LlmCallHookContext,\n"
        "        messages: &mut Vec<ChatMessage>,\n"
        "        model: &mut String,\n"
        "    ) -> HookResult<()> {\n"
        "        self.before_llm_call(messages, model).await\n"
        "    }\n"
        "\n"
        "    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {",
    )

    # ── runtime: the context-aware runner ─────────────────────────────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/hooks/runner.rs",
        "use zeroclaw_api::hook::ToolCallHookContext;\n",
        "use zeroclaw_api::hook::{LlmCallHookContext, ToolCallHookContext};\n",
    )

    replace_required(
        source / "crates/zeroclaw-runtime/src/hooks/runner.rs",
        "    pub async fn run_before_tool_call(\n",
        "    /// Run the before-LLM-call chain with the request's trust split.\n"
        "    ///\n"
        "    /// Same contract as [`Self::run_before_llm_call`] — sequential by\n"
        "    /// priority, short-circuit on `Cancel`, panic-isolated per handler —\n"
        "    /// except that each handler also receives the trust classification of\n"
        "    /// the request.\n"
        "    pub async fn run_before_llm_call_with_context(\n"
        "        &self,\n"
        "        context: &LlmCallHookContext,\n"
        "        messages: &mut Vec<ChatMessage>,\n"
        "        model: &mut String,\n"
        "    ) -> HookResult<()> {\n"
        "        for h in &self.handlers {\n"
        "            let hook_name = h.name();\n"
        "            let mut candidate_messages = messages.clone();\n"
        "            let mut candidate_model = model.clone();\n"
        "            match AssertUnwindSafe(h.before_llm_call_with_context(\n"
        "                context,\n"
        "                &mut candidate_messages,\n"
        "                &mut candidate_model,\n"
        "            ))\n"
        "            .catch_unwind()\n"
        "            .await\n"
        "            {\n"
        "                Ok(HookResult::Continue(())) => {\n"
        "                    *messages = candidate_messages;\n"
        "                    *model = candidate_model;\n"
        "                }\n"
        "                Ok(HookResult::Cancel(reason)) => {\n"
        "                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({\"hook\": hook_name, \"reason\": reason.to_string()})), \"before_llm_call cancelled by hook\");\n"
        "                    return HookResult::Cancel(reason);\n"
        "                }\n"
        "                Err(_) => {\n"
        "                    ::zeroclaw_log::record!(\n"
        "                        ERROR,\n"
        "                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)\n"
        "                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)\n"
        "                            .with_attrs(::serde_json::json!({\"hook\": hook_name})),\n"
        "                        \"before_llm_call hook panicked; continuing with previous values\"\n"
        "                    );\n"
        "                }\n"
        "            }\n"
        "        }\n"
        "        HookResult::Continue(())\n"
        "    }\n"
        "\n"
        "    pub async fn run_before_tool_call(\n",
    )

    # ── runtime: classify the request before the hook chain runs ─────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/hooks/mod.rs",
        "pub mod command;\n",
        "pub mod command;\npub mod trust;\n",
    )

    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/mod.rs",
        "            match hooks\n"
        "                .run_before_llm_call(&mut provider_request_messages, &mut candidate_model)\n"
        "                .await\n",
        "            let trust_context = crate::hooks::trust::classify_messages(\n"
        "                &provider_request_messages,\n"
        "                Some(ingress.trust),\n"
        "            );\n"
        "            match hooks\n"
        "                .run_before_llm_call_with_context(\n"
        "                    &trust_context,\n"
        "                    &mut provider_request_messages,\n"
        "                    &mut candidate_model,\n"
        "                )\n"
        "                .await\n",
    )

    # The max-iteration path has no turn envelope to consult; the classifier
    # still marks tool output and externally-framed payloads untrusted.
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/max_iter.rs",
        "            if let Some(hooks) = hooks.filter(|hooks| !hooks.is_empty())\n"
        "                && let crate::hooks::HookResult::Cancel(reason) = hooks\n"
        "                    .run_before_llm_call(&mut messages, &mut selected_model)\n"
        "                    .await\n",
        "            let trust_context = crate::hooks::trust::classify_messages(&messages, None);\n"
        "            if let Some(hooks) = hooks.filter(|hooks| !hooks.is_empty())\n"
        "                && let crate::hooks::HookResult::Cancel(reason) = hooks\n"
        "                    .run_before_llm_call_with_context(\n"
        "                        &trust_context,\n"
        "                        &mut messages,\n"
        "                        &mut selected_model,\n"
        "                    )\n"
        "                    .await\n",
    )


def project_enact_hooks_docs(source):
    """Ship the hooks documentation page.

    The hook system had no page in `docs/book/src/`, so an operator had no
    written contract for the decision vocabulary, the fail-open rule, or the
    trusted/untrusted split. The page is overlay-authored and copied into the
    prepared source; the book index and the developing index link to it.
    """
    overlay = ROOT / "overlays/enact/native/docs/hooks.md"
    target = source / "docs/book/src/developing/hooks.md"
    if target.exists():
        raise RuntimeError(f"Upstream overlay drift: hooks docs page already exists at {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(overlay, target)

    replace_required(
        source / "docs/book/src/SUMMARY.md",
        "  - [Built-in tool inventory](./developing/tool-inventory.md)\n",
        "  - [Built-in tool inventory](./developing/tool-inventory.md)\n"
        "  - [Hooks](./developing/hooks.md)\n",
    )

    replace_required(
        source / "docs/book/src/developing/index.md",
        "- [Built-in tool inventory](./tool-inventory.md)\n",
        "- [Built-in tool inventory](./tool-inventory.md)\n"
        "- [Hooks](./hooks.md)\n",
    )


def project_enact_context_tools(source):
    """Ship the context and code-intelligence tools.

    Seven new tools and one extended one:

    * `compose` — one call that ranks the workspace against a task and inlines
      the best files, replacing a search-then-read-then-read chain.
    * `expand` — retrieve a tool output that was archived because it was too
      large for the conversation, so any compression stays lossless.
    * `ast_grep` / `ast_edit` — structural code queries and previewed
      structural rewrites, driven by the ast-grep binary.
    * `callgraph` / `graph` — blast-radius analysis at symbol and file
      granularity.
    * `radar` — context budget attribution: where the window actually went.
    * `file_read` read modes — signatures, outline, diff, anchored, task.

    The runtime gains two small modules they share: the tool-output archive
    and the context ledger.

    Every file is overlay-authored and copied into the prepared source, so no
    tracked `src/` or `crates/` Rust changes and no `Cargo.toml` / `Cargo.lock`
    churn.
    """
    overlay = ROOT / "overlays/enact/native/tools"
    tools = source / "crates/zeroclaw-runtime/src/tools"

    for name in (
        "ast_grep",
        "callgraph",
        "compose",
        "expand",
        "file_read_modes",
        "radar",
        "workspace_scan",
    ):
        target = tools / f"{name}.rs"
        if target.exists():
            raise RuntimeError(f"Upstream overlay drift: tool module already exists at {target}")
        shutil.copy2(overlay / f"{name}.rs", target)

    for name, destination in (
        ("tool_archive", "crates/zeroclaw-runtime/src/tool_archive.rs"),
        ("context_ledger", "crates/zeroclaw-runtime/src/context_ledger.rs"),
    ):
        target = source / destination
        if target.exists():
            raise RuntimeError(f"Upstream overlay drift: module already exists at {target}")
        shutil.copy2(overlay / f"{name}.rs", target)

    runtime_tools = source / "crates/zeroclaw-runtime/src/tools/mod.rs"

    # ── module declarations, in the file's alphabetical order ────────────
    replace_required(
        runtime_tools,
        "pub mod cron_add;\n",
        "pub mod ast_grep;\npub mod callgraph;\npub mod compose;\npub mod cron_add;\n",
    )
    replace_required(
        runtime_tools,
        "pub mod deliver_file;\n",
        "pub mod deliver_file;\npub mod expand;\n",
    )
    replace_required(
        runtime_tools,
        "pub mod file_read;\n",
        "pub mod file_read;\npub mod file_read_modes;\n",
    )
    replace_required(
        runtime_tools,
        "pub mod scoped;\n",
        "pub mod radar;\npub mod scoped;\n",
    )
    replace_required(
        runtime_tools,
        "pub mod verifiable_intent;\n",
        "pub mod verifiable_intent;\npub mod workspace_scan;\n",
    )

    # ── re-exports ───────────────────────────────────────────────────────
    replace_required(
        runtime_tools,
        "pub use file_read::FileReadTool;\n",
        "pub use ast_grep::{AstEditTool, AstGrepTool};\n"
        "pub use callgraph::{CallGraphTool, GraphTool};\n"
        "pub use compose::ComposeTool;\n"
        "pub use expand::ExpandTool;\n"
        "pub use file_read::FileReadTool;\n"
        "pub use radar::RadarTool;\n",
    )

    # ── attribution: the new tools are first-party native code ───────────
    attribution = source / "crates/zeroclaw-runtime/src/tools/attribution.rs"
    replace_required(
        attribution,
        "use crate::tools::cron_add::CronAddTool;\n",
        "use crate::tools::ast_grep::{AstEditTool, AstGrepTool};\n"
        "use crate::tools::callgraph::{CallGraphTool, GraphTool};\n"
        "use crate::tools::compose::ComposeTool;\n"
        "use crate::tools::cron_add::CronAddTool;\n"
        "use crate::tools::expand::ExpandTool;\n"
        "use crate::tools::radar::RadarTool;\n",
    )
    replace_required(
        attribution,
        "tool_attribution!(CronAddTool, ToolKind::Plugin);\n",
        "tool_attribution!(AstEditTool, ToolKind::Plugin);\n"
        "tool_attribution!(AstGrepTool, ToolKind::Plugin);\n"
        "tool_attribution!(CallGraphTool, ToolKind::Plugin);\n"
        "tool_attribution!(ComposeTool, ToolKind::Plugin);\n"
        "tool_attribution!(CronAddTool, ToolKind::Plugin);\n"
        "tool_attribution!(ExpandTool, ToolKind::Plugin);\n"
        "tool_attribution!(GraphTool, ToolKind::Plugin);\n"
        "tool_attribution!(RadarTool, ToolKind::Plugin);\n",
    )

    # ── registration in the full registry ────────────────────────────────
    replace_required(
        runtime_tools,
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n',
        '    register_tool!("compose", Arc::new(ComposeTool::new(security.clone())));\n'
        '    register_tool!("expand", Arc::new(ExpandTool::new(security.clone())));\n'
        '    register_tool!("ast_grep", Arc::new(AstGrepTool::new(security.clone())));\n'
        '    register_tool!("ast_edit", Arc::new(AstEditTool::new(security.clone())));\n'
        '    register_tool!("callgraph", Arc::new(CallGraphTool::new(security.clone())));\n'
        '    register_tool!("graph", Arc::new(GraphTool::new(security.clone())));\n'
        '    register_tool!("radar", Arc::new(RadarTool::new()));\n'
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n',
    )

    # ── the two runtime modules ──────────────────────────────────────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/lib.rs",
        "pub mod cost;\n",
        "pub mod context_ledger;\npub mod cost;\n",
    )
    replace_required(
        source / "crates/zeroclaw-runtime/src/lib.rs",
        "pub mod tools;\n",
        "pub mod tool_archive;\npub mod tools;\n",
    )

    # ── file_read read modes ─────────────────────────────────────────────
    file_read = source / "crates/zeroclaw-runtime/src/tools/file_read.rs"
    replace_required(
        file_read,
        '                "encoding": {\n',
        '                "mode": {\n'
        '                    "type": "string",\n'
        '                    "enum": ["full", "signatures", "outline", "diff", "anchored", "task"],\n'
        '                    "description": "How to read the file. \'full\' (default) reads every line. \'signatures\' lists the declarations. \'outline\' shows the structure. \'diff\' compares against \'base\' or \'base_path\'. \'anchored\' shows the lines around \'anchor\'. \'task\' shows the lines that mention \'task\'."\n'
        '                },\n'
        '                "anchor": {\n'
        '                    "type": "string",\n'
        '                    "description": "The text to find, for mode=\'anchored\'."\n'
        '                },\n'
        '                "task": {\n'
        '                    "type": "string",\n'
        '                    "description": "What to look for, for mode=\'task\'."\n'
        '                },\n'
        '                "base": {\n'
        '                    "type": "string",\n'
        '                    "description": "The previous text, for mode=\'diff\'."\n'
        '                },\n'
        '                "base_path": {\n'
        '                    "type": "string",\n'
        '                    "description": "The previous file, for mode=\'diff\'."\n'
        '                },\n'
        '                "context": {\n'
        '                    "type": "integer",\n'
        '                    "description": "Lines of context around an anchor or a task match (default 3)."\n'
        '                },\n'
        '                "encoding": {\n',
    )

    replace_required(
        file_read,
        "        match tokio::fs::read_to_string(&resolved_path).await {\n"
        "            Ok(contents) => {\n",
        "        match tokio::fs::read_to_string(&resolved_path).await {\n"
        "            Ok(contents) => {\n"
        "                match crate::tools::file_read_modes::parse_mode(\n"
        "                    args.get(\"mode\").and_then(|v| v.as_str()),\n"
        "                ) {\n"
        "                    Ok(Some(mode)) => {\n"
        "                        let mut mode_args = args.clone();\n"
        "                        if let Some(base_path) = args.get(\"base_path\").and_then(|v| v.as_str()) {\n"
        "                            let base_full = self.resolve_candidate(base_path)?;\n"
        "                            let base_resolved = tokio::fs::canonicalize(&base_full).await.map_err(|e| {\n"
        "                                anyhow::Error::msg(format!(\"Failed to resolve base path: {e}\"))\n"
        "                            })?;\n"
        "                            if !self.security.is_resolved_path_readable(&base_resolved) {\n"
        "                                anyhow::bail!(\"Path escapes workspace directory: {base_path}\");\n"
        "                            }\n"
        "                            let base_text = tokio::fs::read_to_string(&base_resolved).await.map_err(|e| {\n"
        "                                anyhow::Error::msg(format!(\"Failed to read base file: {e}\"))\n"
        "                            })?;\n"
        "                            mode_args[\"base\"] = serde_json::Value::String(base_text);\n"
        "                        }\n"
        "                        return Ok(crate::tools::file_read_modes::render(\n"
        "                            mode,\n"
        "                            &contents,\n"
        "                            &resolved_path,\n"
        "                            &mode_args,\n"
        "                        ));\n"
        "                    }\n"
        "                    Ok(None) => {}\n"
        "                    Err(message) => {\n"
        "                        return Ok(ToolResult {\n"
        "                            success: false,\n"
        "                            output: ToolOutput::default(),\n"
        "                            error: Some(message),\n"
        "                        });\n"
        "                    }\n"
        "                }\n",
    )

    # ── the tool-output archive ──────────────────────────────────────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/post_exec.rs",
        "        // ── Progress: tool completion ───────────────────────\n",
        "        // ── Archive: a result too large for the conversation ───────\n"
        "        // The full text goes to the archive and the model keeps a head\n"
        "        // plus a handle, so `expand` can give the rest back later.\n"
        "        if outcome.output.chars().count() > crate::tool_archive::ARCHIVE_THRESHOLD_CHARS\n"
        "            && let Some(handle) = crate::tool_archive::archive(&call.name, &outcome.output)\n"
        "        {\n"
        "            let archived = crate::tool_archive::archived_text(&handle, &outcome.output);\n"
        "            outcome.output = archived;\n"
        "        }\n"
        "\n"
        "        // ── Progress: tool completion ───────────────────────\n",
    )

    # ── the context ledger ──────────────────────────────────────────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/mod.rs",
        "                crate::hooks::HookResult::Cancel(reason) => {\n"
        "                    anyhow::bail!(\"LLM call cancelled by hook: {reason}\");\n"
        "                }\n"
        "            }\n"
        "        }\n",
        "                crate::hooks::HookResult::Cancel(reason) => {\n"
        "                    anyhow::bail!(\"LLM call cancelled by hook: {reason}\");\n"
        "                }\n"
        "            }\n"
        "            // Account for where this request's context went, so `radar`\n"
        "            // can say which source spent the window.\n"
        "            crate::context_ledger::record(crate::context_ledger::account(\n"
        "                turn_id,\n"
        "                agent_alias,\n"
        "                &provider_request_messages,\n"
        "                injected_memory_preamble.as_ref().map(|preamble| preamble.index),\n"
        "            ));\n"
        "        }\n",
    )

    # ── the inventory rows ───────────────────────────────────────────────
    inventory = source / "crates/zeroclaw-tools/src/inventory.rs"
    replace_required(
        inventory,
        "    // Tier 3: optional. Utilities.\n",
        "    // Tier 2: host-coupled. Context and code intelligence.\n"
        + "".join(
            "    BuiltinToolSpec {\n"
            f'        name: "{name}",\n'
            "        tier: ToolTier::Host,\n"
            "        compilation: BuiltinToolCompilation::Always,\n"
            "    },\n"
            for name in (
                "compose",
                "expand",
                "ast_grep",
                "ast_edit",
                "callgraph",
                "graph",
                "radar",
            )
        )
        + "    // Tier 3: optional. Utilities.\n",
    )

    # ── the inventory docs page ──────────────────────────────────────────
    replace_required(
        source / "docs/book/src/developing/tool-inventory.md",
        "| Runtime-defined | `security_ops` | Lives in the runtime crate and uses its security playbook and vulnerability modules. |\n",
        "| Runtime-defined | `security_ops` | Lives in the runtime crate and uses its security playbook and vulnerability modules. |\n"
        "| Context and code intelligence | `compose`, `expand`, `ast_grep`, `ast_edit`, `callgraph`, `graph`, `radar` | The context-assembly and code-intelligence surface. `compose` ranks the workspace against a task and inlines it; `expand` retrieves a tool output that was archived for being too large; `ast_grep` and `ast_edit` drive the ast-grep binary for structural queries and previewed rewrites; `callgraph` and `graph` answer the blast-radius question a content search cannot; `radar` reports where the context window went. They run behind the same path guard as the file tools. |\n",
    )


def project_enact_tool_families(source):
    """Consolidate the sibling tools into canonical family tools.

    Thirty-six sibling tools used together all the time were thirty-six
    entries in the model's tool list. Each family below collapses into one
    canonical tool with a typed `action` enum — `cron` with `add`, `list`,
    `remove`, `update`, `run`, `runs`, `schedule`.

    The legacy names keep working. A call to `cron_add` still schedules a
    job, and it carries a one-time notice naming the canonical tool and the
    action that replaces it. The notice travels beside the result body, in
    the structured data, so the body stays exactly what the tool produced.
    """
    overlay = ROOT / "overlays/enact/native/tools/families.rs"
    target = source / "crates/zeroclaw-runtime/src/tools/families.rs"
    if target.exists():
        raise RuntimeError(f"Upstream overlay drift: families module already exists at {target}")
    shutil.copy2(overlay, target)

    # The family table must agree with the operations inventory and with the
    # tools it describes, or the native surface and the canonical registry
    # would offer different actions under the same name.
    check_enact_family_members(source)

    runtime_tools = source / "crates/zeroclaw-runtime/src/tools/mod.rs"

    replace_required(
        runtime_tools,
        "pub mod file_read;\n",
        "pub mod families;\npub mod file_read;\n",
    )
    replace_required(
        runtime_tools,
        "pub use file_read::FileReadTool;\n",
        "pub use families::FamilyTool;\npub use file_read::FileReadTool;\n",
    )

    # ── register the family tools beside their members ──────────────────
    replace_required(
        runtime_tools,
        "    let tools = native_memory.wrap_actual_tools(&tool_arcs);\n    Ok(AllToolsResult {\n",
        "    // The consolidated families: one canonical tool per family, with a\n"
        "    // typed action enum over the legacy members. The members stay\n"
        "    // registered; the family is the name the model should learn.\n"
        "    for (family_name, family_tool) in crate::tools::families::build_all(&tool_arcs) {\n"
        "        register_builtin_tool(\n"
        "            &root_config.tools,\n"
        "            &mut tool_arcs,\n"
        "            &mut reserved_host_names,\n"
        "            family_name,\n"
        "            || Arc::new(family_tool),\n"
        "        );\n"
        "    }\n"
        "\n"
        "    let tools = native_memory.wrap_actual_tools(&tool_arcs);\n"
        "    Ok(AllToolsResult {\n",
    )

    # ── the one-time deprecation notice ─────────────────────────────────
    tool_execution = source / "crates/zeroclaw-runtime/src/agent/tool_execution.rs"
    replace_required(
        tool_execution,
        "                    Ok(ToolExecutionOutcome {\n"
        "                        output: normalized_output.to_string(),\n"
        "                        attachments: r.output.attachments().to_vec(),\n"
        "                        output_data: r.output.into_data(),\n",
        "                    // Take the body text before the output is consumed:\n"
        "                    // the notice travels in the structured data, and the\n"
        "                    // body stays exactly what the tool produced.\n"
        "                    let output_text = normalized_output.to_string();\n"
        "                    let attachments = r.output.attachments().to_vec();\n"
        "                    let mut output_data = r.output.into_data();\n"
        "                    // A legacy tool name carries a one-time notice\n"
        "                    // naming its canonical form, beside the result body.\n"
        "                    crate::tools::families::attach_deprecation_notice(\n"
        "                        call_name,\n"
        "                        &mut output_data,\n"
        "                    );\n"
        "                    Ok(ToolExecutionOutcome {\n"
        "                        output: output_text,\n"
        "                        attachments,\n"
        "                        output_data,\n",
    )

    replace_required(
        tool_execution,
        "                    Ok(ToolExecutionOutcome {\n"
        "                        output: format!(\"Error: {model_visible}\"),\n"
        "                        success: false,\n"
        "                        error_reason: Some(reason),\n"
        "                        duration,\n"
        "                        receipt: None,\n"
        "                        attachments,\n"
        "                        output_data,\n"
        "                    })\n",
        "                    let mut output_data = output_data;\n"
        "                    crate::tools::families::attach_deprecation_notice(\n"
        "                        call_name,\n"
        "                        &mut output_data,\n"
        "                    );\n"
        "                    Ok(ToolExecutionOutcome {\n"
        "                        output: format!(\"Error: {model_visible}\"),\n"
        "                        success: false,\n"
        "                        error_reason: Some(reason),\n"
        "                        duration,\n"
        "                        receipt: None,\n"
        "                        attachments,\n"
        "                        output_data,\n"
        "                    })\n",
    )

    # ── the MCP surface renders the notice beside the body ─────────────
    # The notice support is baked into the overlay-owned MCP module, not
    # patched into it here: the module is ours, so it ships in final form.
    # Patching our own overlay in place would not be idempotent — a second
    # `prepare` would find the anchor already replaced.
    protocol = ROOT / "overlays/enact/native/mcp-server/src/protocol.rs"
    mcp_server = ROOT / "overlays/enact/native/mcp-server/src/server.rs"
    for path, needle in (
        (protocol, "pub fn tool_result_with_notice("),
        (mcp_server, "tool_result_with_notice(text, &notice)"),
    ):
        text = path.read_text()
        if text.count(needle) != 1:
            raise RuntimeError(
                f"Upstream overlay drift: expected exactly one {needle!r} in {path}"
            )

    # ── the inventory rows ──────────────────────────────────────────────
    inventory = source / "crates/zeroclaw-tools/src/inventory.rs"
    replace_required(
        inventory,
        "    // Tier 3: optional. Utilities.\n",
        "    // Tier 2: host-coupled. The consolidated families.\n"
        + "".join(
            "    BuiltinToolSpec {\n"
            f'        name: "{name}",\n'
            "        tier: ToolTier::Host,\n"
            "        compilation: BuiltinToolCompilation::Always,\n"
            "    },\n"
            for name in (
                "cron",
                "sop",
                "memory",
                "a2a",
                "sessions",
                "model",
                "mcp",
                "cloud",
                "file_uploads",
                "email",
            )
        )
        + "    // Tier 3: optional. Utilities.\n",
    )

    # ── the inventory docs page ──────────────────────────────────────────
    replace_required(
        source / "docs/book/src/developing/tool-inventory.md",
        "| Context and code intelligence | `compose`, `expand`, `ast_grep`, `ast_edit`, `callgraph`, `graph`, `radar` |",
        "| Consolidated families | `cron`, `sop`, `memory`, `a2a`, `sessions`, `model`, `mcp`, `cloud`, `file_uploads`, `email` | Each family is one canonical tool with a typed `action` enum over the sibling tools it replaces. The legacy names keep working and carry a one-time notice naming the canonical form. |\n"
        "| Context and code intelligence | `compose`, `expand`, `ast_grep`, `ast_edit`, `callgraph`, `graph`, `radar` |",
    )

    # ── the model sees the family, not the members ───────────────────────
    # The members stay in the registry: a config, a script, or a model that
    # remembers the old name can still call them, and the call carries a
    # one-time notice. What the model is *offered* is the family, so the
    # consolidation actually removes the thirty-six entries from the tool
    # list instead of adding ten to it.
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/tool_specs.rs",
        "    let mut tool_specs: Vec<crate::tools::ToolSpec> = tools_registry\n"
        "        .iter()\n"
        "        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))\n",
        "    let mut tool_specs: Vec<crate::tools::ToolSpec> = tools_registry\n"
        "        .iter()\n"
        "        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))\n"
    )

    # ── the family tools that need a live server to exist ────────────────
    # The `mcp` family collapses `mcp_resources` and `mcp_prompts`, which are
    # only built when MCP is enabled and a server advertises something. The
    # family therefore cannot be constructed in a maximal-config unit test,
    # for the same reason upstream allowlists `tool_search`.
    replace_required(
        source / "crates/zeroclaw-runtime/src/tools/mod.rs",
        '        let unconstructible = std::collections::BTreeMap::from([(\n'
        '            "tool_search",\n',
        '        let unconstructible = std::collections::BTreeMap::from([(\n'
        '            "mcp",\n'
        '            "the family collapses `mcp_resources` and `mcp_prompts`, which are \\\n'
        '             built only when MCP is enabled and a connected server advertises \\\n'
        '             at least one tool",\n'
        '        ), (\n'
        '            "tool_search",\n',
    )


def project_enact_coordination(source):
    """Ship the coordination primitives: `wait`, and the approval tiers.

    `wait` is the blocking form of "what happened to that background task":
    name the task, get the outcome when it lands, with a bounded budget so a
    stuck task cannot hang the turn forever. It reads the durable task
    registry the delegate and subagent producers already write to, so it adds
    no new lifecycle state.

    The approval tiers give the operator a vocabulary that does not rot as
    the tool list changes: read, write, exec. A policy entry may name a tier
    — `auto_approve = ["read"]` — instead of a list of readers. A tier never
    widens what the policy already allows; it only lets the policy be
    written in terms of what a tool does.

    A consolidated family is judged by the member its action names, so
    `memory` with the `recall` action carries the policy of
    `memory_recall`: the consolidation does not quietly change what an
    operator's configuration means.
    """
    runtime_tools = source / "crates/zeroclaw-runtime/src/tools/mod.rs"

    # ── the approval tiers ─────────────────────────────────────────────
    overlay = ROOT / "overlays/enact/native/approval/tiers.rs"
    target = source / "crates/zeroclaw-runtime/src/approval/tiers.rs"
    if target.exists():
        raise RuntimeError(
            f"Upstream overlay drift: the approval tiers module already exists at {target}"
        )
    shutil.copy2(overlay, target)

    approval = source / "crates/zeroclaw-runtime/src/approval/mod.rs"
    replace_required(
        approval,
        "use crate::security::AutonomyLevel;\n",
        "pub mod tiers;\n\n"
        "use crate::approval::tiers::ApprovalTier;\n"
        "use crate::security::AutonomyLevel;\n",
    )
    replace_required(
        approval,
        "    /// Check whether a tool call requires interactive approval.\n"
        "    /// Returns `true` if the call needs a prompt, `false` if it can proceed.\n"
        "    pub fn needs_approval(&self, tool_name: &str) -> bool {\n"
        "        self.approval_requirement(tool_name) == ApprovalRequirement::Prompt\n"
        "    }\n"
        "\n"
        "    pub fn approval_requirement(&self, tool_name: &str) -> ApprovalRequirement {\n"
        "        // ReadOnly blocks everything — handled elsewhere; no prompt needed.\n"
        "        if self.autonomy_level == AutonomyLevel::ReadOnly {\n"
        "            return ApprovalRequirement::NotRequired;\n"
        "        }\n"
        "\n"
        "        // always_ask overrides everything, including Full autonomy — an operator\n"
        "        // who explicitly lists a tool here wants a prompt regardless of level.\n"
        "        if self.always_ask.contains(\"*\") || self.always_ask.contains(tool_name) {\n"
        "            return ApprovalRequirement::Prompt;\n"
        "        }\n"
        "\n"
        "        // Full autonomy auto-approves only tools that are not always_ask.\n"
        "        if self.autonomy_level == AutonomyLevel::Full {\n"
        "            return ApprovalRequirement::Approved;\n"
        "        }\n"
        "\n"
        "        if self.non_interactive\n"
        "            && tool_name == \"shell\"\n"
        "            && !self.non_interactive_shell_requires_approval\n"
        "        {\n"
        "            return ApprovalRequirement::NotRequired;\n"
        "        }\n"
        "\n"
        "        // auto_approve skips the prompt.\n"
        "        if self.auto_approve.contains(\"*\") || self.auto_approve.contains(tool_name) {\n"
        "            return ApprovalRequirement::Approved;\n"
        "        }\n"
        "\n"
        "        // Session allowlist (from prior \"Always\" responses).\n"
        "        let allowlist = self.session_allowlist.lock();\n"
        "        if allowlist.contains(tool_name) {\n"
        "            return ApprovalRequirement::Approved;\n"
        "        }\n"
        "\n"
        "        // Default: supervised mode requires approval.\n"
        "        ApprovalRequirement::Prompt\n"
        "    }\n",
        "    /// Check whether a tool call requires interactive approval.\n"
        "    /// Returns `true` if the call needs a prompt, `false` if it can proceed.\n"
        "    pub fn needs_approval(&self, tool_name: &str) -> bool {\n"
        "        self.approval_requirement(tool_name) == ApprovalRequirement::Prompt\n"
        "    }\n"
        "\n"
        "    /// Check whether a call with an action requires interactive approval.\n"
        "    ///\n"
        "    /// A consolidated family is judged by the member its action names, so\n"
        "    /// `memory` with the `recall` action carries the policy of\n"
        "    /// `memory_recall`.\n"
        "    pub fn needs_approval_for(&self, tool_name: &str, action: Option<&str>) -> bool {\n"
        "        self.approval_requirement_for(tool_name, action) == ApprovalRequirement::Prompt\n"
        "    }\n"
        "\n"
        "    /// The tier a call falls into: read, write, or exec.\n"
        "    pub fn approval_tier(&self, tool_name: &str, action: Option<&str>) -> ApprovalTier {\n"
        "        crate::approval::tiers::tier_for_call(tool_name, action)\n"
        "    }\n"
        "\n"
        "    pub fn approval_requirement(&self, tool_name: &str) -> ApprovalRequirement {\n"
        "        self.approval_requirement_for(tool_name, None)\n"
        "    }\n"
        "\n"
        "    /// The approval requirement for one call.\n"
        "    ///\n"
        "    /// The names the call may be judged by are the canonical tool and, for\n"
        "    /// a consolidated family, the member the action names. The tier is the\n"
        "    /// member's, so a policy entry may name a tier instead of a tool.\n"
        "    pub fn approval_requirement_for(\n"
        "        &self,\n"
        "        tool_name: &str,\n"
        "        action: Option<&str>,\n"
        "    ) -> ApprovalRequirement {\n"
        "        // ReadOnly blocks everything — handled elsewhere; no prompt needed.\n"
        "        if self.autonomy_level == AutonomyLevel::ReadOnly {\n"
        "            return ApprovalRequirement::NotRequired;\n"
        "        }\n"
        "\n"
        "        let names = self.policy_names(tool_name, action);\n"
        "        let tier = crate::approval::tiers::tier_for_call(tool_name, action);\n"
        "\n"
        "        // always_ask overrides everything, including Full autonomy — an operator\n"
        "        // who explicitly lists a tool here wants a prompt regardless of level.\n"
        "        if self.matches_policy(&self.always_ask, &names, tier) {\n"
        "            return ApprovalRequirement::Prompt;\n"
        "        }\n"
        "\n"
        "        // Full autonomy auto-approves only tools that are not always_ask.\n"
        "        if self.autonomy_level == AutonomyLevel::Full {\n"
        "            return ApprovalRequirement::Approved;\n"
        "        }\n"
        "\n"
        "        if self.non_interactive\n"
        "            && tool_name == \"shell\"\n"
        "            && !self.non_interactive_shell_requires_approval\n"
        "        {\n"
        "            return ApprovalRequirement::NotRequired;\n"
        "        }\n"
        "\n"
        "        // auto_approve skips the prompt.\n"
        "        if self.matches_policy(&self.auto_approve, &names, tier) {\n"
        "            return ApprovalRequirement::Approved;\n"
        "        }\n"
        "\n"
        "        // Session allowlist (from prior \"Always\" responses).\n"
        "        let allowlist = self.session_allowlist.lock();\n"
        "        if names.iter().any(|name| allowlist.contains(name)) {\n"
        "            return ApprovalRequirement::Approved;\n"
        "        }\n"
        "\n"
        "        // Default: supervised mode requires approval.\n"
        "        ApprovalRequirement::Prompt\n"
        "    }\n"
        "\n"
        "    /// The names a call may be judged by.\n"
        "    ///\n"
        "    /// A consolidated family is judged by its canonical name and by the\n"
        "    /// member tool the action names: an operator who listed `memory_recall`\n"
        "    /// must keep approving nothing for a read, whether the model spells it\n"
        "    /// `memory_recall` or `memory` with the `recall` action.\n"
        "    fn policy_names(&self, tool_name: &str, action: Option<&str>) -> Vec<String> {\n"
        "        let mut names = vec![tool_name.to_string()];\n"
        "        if let Some((member, _)) = crate::tools::families::resolve(tool_name, action)\n"
        "            && member != tool_name\n"
        "        {\n"
        "            names.push(member.to_string());\n"
        "        }\n"
        "        names\n"
        "    }\n"
        "\n"
        "    /// Whether a policy list covers a call.\n"
        "    ///\n"
        "    /// A policy entry names a tool, a tier, or `*`. A tier entry covers every\n"
        "    /// call in that tier, so an operator can write `auto_approve = [\"read\"]`\n"
        "    /// instead of maintaining a list of readers.\n"
        "    fn matches_policy(\n"
        "        &self,\n"
        "        entries: &HashSet<String>,\n"
        "        names: &[String],\n"
        "        tier: ApprovalTier,\n"
        "    ) -> bool {\n"
        "        if entries.contains(\"*\") {\n"
        "            return true;\n"
        "        }\n"
        "        if names.iter().any(|name| entries.contains(name)) {\n"
        "            return true;\n"
        "        }\n"
        "        entries\n"
        "            .iter()\n"
        "            .any(|entry| ApprovalTier::parse(entry) == Some(tier))\n"
        "    }\n",
    )
    replace_required(
        approval,
        "            && !self.always_ask.contains(\"*\")\n"
        "            && !self.always_ask.contains(tool_name)\n",
        "            && !self.always_ask.contains(\"*\")\n"
        "            && !self.always_ask.contains(tool_name)\n"
        "            && !self.always_ask.iter().any(|entry| {\n"
        "                ApprovalTier::parse(entry)\n"
        "                    == Some(crate::approval::tiers::tier_for(tool_name))\n"
        "            })\n",
    )

    # ── the call sites know the action ─────────────────────────────────
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/approval_gate.rs",
        "    let mut approval_requirement = ctx\n"
        "        .approval\n"
        "        .map(|mgr| mgr.approval_requirement(tool_name))\n"
        "        .unwrap_or(ApprovalRequirement::NotRequired);\n",
        "    let mut approval_requirement = ctx\n"
        "        .approval\n"
        "        .map(|mgr| {\n"
        "            mgr.approval_requirement_for(\n"
        "                tool_name,\n"
        "                tool_args.get(\"action\").and_then(|value| value.as_str()),\n"
        "            )\n"
        "        })\n"
        "        .unwrap_or(ApprovalRequirement::NotRequired);\n",
    )
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/turn/call_prep.rs",
        "        let requires_prompt = ctx\n"
        "            .approval\n"
        "            .map(|mgr| mgr.needs_approval(&tool_name))\n"
        "            .unwrap_or(false);\n",
        "        let requires_prompt = ctx\n"
        "            .approval\n"
        "            .map(|mgr| {\n"
        "                mgr.needs_approval_for(\n"
        "                    &tool_name,\n"
        "                    tool_args.get(\"action\").and_then(|value| value.as_str()),\n"
        "                )\n"
        "            })\n"
        "            .unwrap_or(false);\n",
    )
    replace_required(
        source / "crates/zeroclaw-runtime/src/agent/tool_execution.rs",
        "    if let Some(mgr) = approval\n"
        "        && tool_calls.iter().any(|call| mgr.needs_approval(&call.name))\n"
        "    {\n",
        "    if let Some(mgr) = approval\n"
        "        && tool_calls.iter().any(|call| {\n"
        "            mgr.needs_approval_for(\n"
        "                &call.name,\n"
        "                call.arguments.get(\"action\").and_then(|value| value.as_str()),\n"
        "            )\n"
        "        })\n"
        "    {\n",
    )

    # ── the `wait` tool ────────────────────────────────────────────────
    overlay = ROOT / "overlays/enact/native/tools/wait_task.rs"
    target = source / "crates/zeroclaw-runtime/src/tools/wait_task.rs"
    if target.exists():
        raise RuntimeError(f"Upstream overlay drift: wait module already exists at {target}")
    shutil.copy2(overlay, target)

    replace_required(
        runtime_tools,
        "pub mod verifiable_intent;\n",
        "pub mod verifiable_intent;\npub mod wait_task;\n",
    )
    replace_required(
        runtime_tools,
        "pub use file_read::FileReadTool;\n",
        "pub use file_read::FileReadTool;\npub use wait_task::WaitTool;\n",
    )
    replace_required(
        runtime_tools,
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n',
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n'
        "    // `wait` blocks on the durable task registry the background\n"
        "    // producers write to. The store is opened on demand, so the tool\n"
        "    // exists in every configuration.\n"
        "    register_tool!(\n"
        '        "wait",\n'
        "        Arc::new(WaitTool::from_data_dir(\n"
        "            root_config.data_dir.clone(),\n"
        "            Some(agent_alias.to_string()),\n"
        "        ))\n"
        "    );\n",
    )

    # ── attribution: the new tool is first-party native code ───────────
    attribution = source / "crates/zeroclaw-runtime/src/tools/attribution.rs"
    replace_required(
        attribution,
        "use crate::tools::verifiable_intent::VerifiableIntentTool;\n",
        "use crate::tools::verifiable_intent::VerifiableIntentTool;\n"
        "use crate::tools::wait_task::WaitTool;\n",
    )
    replace_required(
        attribution,
        "tool_attribution!(VerifiableIntentTool, ToolKind::Plugin);\n",
        "tool_attribution!(VerifiableIntentTool, ToolKind::Plugin);\n"
        "tool_attribution!(WaitTool, ToolKind::Plugin);\n",
    )

    # ── the `learn` tool ───────────────────────────────────────────────
    overlay = ROOT / "overlays/enact/native/tools/learn.rs"
    target = source / "crates/zeroclaw-runtime/src/tools/learn.rs"
    if target.exists():
        raise RuntimeError(f"Upstream overlay drift: learn module already exists at {target}")
    shutil.copy2(overlay, target)

    replace_required(
        runtime_tools,
        "pub mod file_read;\n",
        "pub mod file_read;\npub mod learn;\n",
    )
    replace_required(
        runtime_tools,
        "pub use file_read::FileReadTool;\n",
        "pub use file_read::FileReadTool;\npub use learn::LearnTool;\n",
    )
    replace_required(
        runtime_tools,
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n',
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n'
        "    // The skill surface: list the installed skills, read one, and\n"
        "    // change one. `skill_manage` refuses to change anything while\n"
        "    // skill improvement is disabled, so the operator's configuration\n"
        "    // still decides whether a skill may be rewritten.\n"
        "    register_tool!(\n"
        '        "skills_list",\n'
        "        Arc::new(crate::tools::skill_manage::SkillsListTool::new(\n"
        "            workspace_dir.to_path_buf(),\n"
        "        ))\n"
        "    );\n"
        "    register_tool!(\n"
        '        "skill_view",\n'
        "        Arc::new(crate::tools::skill_manage::SkillViewTool::new(\n"
        "            workspace_dir.to_path_buf(),\n"
        "        ))\n"
        "    );\n"
        "    register_tool!(\n"
        '        "skill_manage",\n'
        "        Arc::new(crate::tools::skill_manage::SkillManageTool::new(\n"
        "            workspace_dir.to_path_buf(),\n"
        "            root_config.skills.skill_improvement.clone(),\n"
        "            root_config.skills.allow_scripts,\n"
        "        ))\n"
        "    );\n"
        "    // `learn` records what the agent learned, so the next run starts\n"
        "    // from it instead of from zero.\n"
        "    register_tool!(\n"
        '        "learn",\n'
        "        Arc::new(LearnTool::new(workspace_dir.to_path_buf()))\n"
        "    );\n",
    )
    replace_required(
        attribution,
        "use crate::tools::verifiable_intent::VerifiableIntentTool;\n",
        "use crate::tools::learn::LearnTool;\n"
        "use crate::tools::verifiable_intent::VerifiableIntentTool;\n",
    )
    replace_required(
        attribution,
        "tool_attribution!(VerifiableIntentTool, ToolKind::Plugin);\n",
        "tool_attribution!(LearnTool, ToolKind::Plugin);\n"
        "tool_attribution!(VerifiableIntentTool, ToolKind::Plugin);\n",
    )

    # ── the inventory rows ───────────────────────────────────────────────
    inventory = source / "crates/zeroclaw-tools/src/inventory.rs"
    replace_required(
        inventory,
        "    // Tier 3: optional. Utilities.\n",
        "    // Tier 2: host-coupled. Coordination.\n"
        "    BuiltinToolSpec {\n"
        '        name: "wait",\n'
        "        tier: ToolTier::Host,\n"
        "        compilation: BuiltinToolCompilation::Always,\n"
        "    },\n"
        "    // Tier 2: host-coupled. Skills and lessons.\n"
        + "".join(
            "    BuiltinToolSpec {\n"
            f'        name: "{name}",\n'
            "        tier: ToolTier::Host,\n"
            "        compilation: BuiltinToolCompilation::Always,\n"
            "    },\n"
            for name in ("skills_list", "skill_view", "skill_manage", "learn")
        )
        + "    // Tier 3: optional. Utilities.\n",
    )

    # ── the inventory docs page ──────────────────────────────────────────
    replace_required(
        source / "docs/book/src/developing/tool-inventory.md",
        "| Runtime-defined | `security_ops` | Lives in the runtime crate and uses its security playbook and vulnerability modules. |\n",
        "| Runtime-defined | `security_ops` | Lives in the runtime crate and uses its security playbook and vulnerability modules. |\n"
        "| Coordination | `wait` | Blocks on the durable task registry until a background task reaches a terminal state, with a bounded timeout. It reads the same registry the delegate and subagent producers write to, and it adds no new lifecycle state. |\n"
        "| Skills and lessons | `skills_list`, `skill_view`, `skill_manage`, `learn` | `skills_list` and `skill_view` read the installed skills; `skill_manage` rewrites one, and refuses to while skill improvement is disabled; `learn` records a lesson so the next run starts from it, optionally folding it into a named skill. |\n",
    )

    # ── the skill tools are in the inventory now ─────────────────────────
    scan = source / "crates/zeroclaw-tools/tests/inventory_source_scan.rs"
    for tool in ("skills_list", "skill_view", "skill_manage"):
        remove_required(
            scan,
            "    (\n"
            f'        "{tool}",\n'
            '        "only the opt-in background skill review registers it",\n'
            "    ),\n",
        )


def project_enact_turn_control(source):
    """Give the turn a control channel: plan mode, checkpoints, and rewinds.

    A tool sees only its own arguments — the transcript belongs to the turn
    loop — so the capabilities that need the conversation's history cannot be
    built as ordinary tools. The loop owns a control channel and scopes it
    around the turn; the tools borrow it through the channel's task-local and
    ask for a change; the loop applies the change at the top of its next
    iteration, where it owns the transcript.

    Plan mode rides the same channel. While the turn is in plan mode only the
    read-tier tools run, and the write and execute tools are refused with a
    message that says what to do instead. Leaving plan mode is the operator's
    decision: the agent asks with `plan_execute`, which the approval policy
    judges like any other write.
    """
    agent = source / "crates/zeroclaw-runtime/src/agent/mod.rs"
    turn = source / "crates/zeroclaw-runtime/src/agent/turn/mod.rs"
    specs = source / "crates/zeroclaw-runtime/src/agent/turn/tool_specs.rs"
    prep = source / "crates/zeroclaw-runtime/src/agent/turn/call_prep.rs"
    runtime_tools = source / "crates/zeroclaw-runtime/src/tools/mod.rs"
    attribution = source / "crates/zeroclaw-runtime/src/tools/attribution.rs"
    runtime_lib = source / "crates/zeroclaw-runtime/src/lib.rs"

    # The trait solver runs out of room while it proves that an async tool
    # call is `Send`: every `Tool` implementation is a candidate in that
    # search, so the crate's own tool count decides whether the default
    # recursion limit is enough. The limit is raised rather than the proof
    # narrowed, because the proof is the same one the compiler has always
    # made — it just needs more room now.
    replace_required(
        runtime_lib,
        "#![allow(\n"
        "    clippy::to_string_in_format_args,\n"
        "    clippy::useless_format,\n"
        "    clippy::manual_inspect\n"
        ")]\n",
        "#![allow(\n"
        "    clippy::to_string_in_format_args,\n"
        "    clippy::useless_format,\n"
        "    clippy::manual_inspect\n"
        ")]\n"
        "// The trait solver needs more room than the default when it compares\n"
        "// every `Tool` implementation while proving that an async tool call\n"
        "// is `Send`. Each implementation is a candidate in that search.\n"
        "#![recursion_limit = \"512\"]\n",
    )

    # ── the channel and the tools ───────────────────────────────────────
    for relative, target_relative in (
        ("agent/turn_control.rs", "crates/zeroclaw-runtime/src/agent/turn_control.rs"),
        ("tools/checkpoint.rs", "crates/zeroclaw-runtime/src/tools/checkpoint.rs"),
        ("tools/rewind.rs", "crates/zeroclaw-runtime/src/tools/rewind.rs"),
        ("tools/plan_mode.rs", "crates/zeroclaw-runtime/src/tools/plan_mode.rs"),
        ("tools/plan_execute.rs", "crates/zeroclaw-runtime/src/tools/plan_execute.rs"),
    ):
        overlay = ROOT / "overlays/enact/native" / relative
        target = source / target_relative
        if target.exists():
            raise RuntimeError(
                f"Upstream overlay drift: {target_relative} already exists at {target}"
            )
        shutil.copy2(overlay, target)

    replace_required(
        agent,
        "pub mod tool_execution;\npub mod tool_receipts;\n",
        "pub mod tool_execution;\npub mod tool_receipts;\npub mod turn_control;\n",
    )

    # ── the loop owns the channel ───────────────────────────────────────
    # The channel is scoped around the whole turn, so the loop's own code and
    # every tool inside it read the same mode.
    replace_required(
        turn,
        "async fn run_tool_call_loop_impl<'a>(\n"
        "    mut p: ToolLoop<'a>,\n"
        "    initial_ingress: Option<InitialIngressSubject<'a>>,\n"
        "    seed_reported_usage: Option<ReportedRequestUsage>,\n"
        "    repetition_guard: Option<&'a std::sync::Mutex<repetition::RepetitionGuard>>,\n"
        ") -> Result<String> {\n",
        "async fn run_tool_call_loop_impl<'a>(\n"
        "    p: ToolLoop<'a>,\n"
        "    initial_ingress: Option<InitialIngressSubject<'a>>,\n"
        "    seed_reported_usage: Option<ReportedRequestUsage>,\n"
        "    repetition_guard: Option<&'a std::sync::Mutex<repetition::RepetitionGuard>>,\n"
        ") -> Result<String> {\n"
        "    // The turn's control channel: plan mode, checkpoints, and rewinds.\n"
        "    // The loop owns it; the tools borrow it through the scope below and\n"
        "    // ask for a change, and the loop applies what they asked for at the\n"
        "    // top of its next iteration, where it owns the transcript.\n"
        "    let control = crate::agent::turn_control::TurnControl::new(\n"
        "        crate::agent::turn_control::AgentMode::Execute,\n"
        "    );\n"
        "    // The turn engine is a very large future. It is boxed so the\n"
        "    // stack holds a pointer to it rather than its whole state, and\n"
        "    // the scope around it does not put the turn on the stack.\n"
        "    crate::agent::turn_control::scope(\n"
        "        Arc::clone(&control),\n"
        "        Box::pin(run_tool_call_loop_inner(\n"
        "            p,\n"
        "            initial_ingress,\n"
        "            seed_reported_usage,\n"
        "            repetition_guard,\n"
        "            control,\n"
        "        )),\n"
        "    )\n"
        "    .await\n"
        "}\n"
        "\n"
        "async fn run_tool_call_loop_inner<'a>(\n"
        "    mut p: ToolLoop<'a>,\n"
        "    initial_ingress: Option<InitialIngressSubject<'a>>,\n"
        "    seed_reported_usage: Option<ReportedRequestUsage>,\n"
        "    repetition_guard: Option<&'a std::sync::Mutex<repetition::RepetitionGuard>>,\n"
        "    control: crate::agent::turn_control::TurnControlHandle,\n"
        ") -> Result<String> {\n",
    )

    # The transcript's position, as a checkpoint records it and a rewind
    # restores it. Both buffers have to move together: the provider-visible
    # transcript and the persistence delta grow in step, and the post-turn
    # replay puts the canonical buffer back into the durable history.
    replace_required(
        turn,
        "    /// Advance the cursor past all current canonical messages.  Call after\n"
        "    /// external code modifies both buffers (e.g. `drive_live_sop_actions`).\n",
        "    /// Where the transcript stands: the provider-visible length and the\n"
        "    /// persistence-delta length. A checkpoint records this pair, so a\n"
        "    /// rewind can restore both buffers exactly.\n"
        "    fn anchor(&self) -> crate::agent::turn_control::TranscriptAnchor {\n"
        "        crate::agent::turn_control::TranscriptAnchor {\n"
        "            history_len: self.history.len(),\n"
        "            canonical_len: self.canonical.as_ref().map_or(0, |c| c.len()),\n"
        "        }\n"
        "    }\n"
        "\n"
        "    /// Restore both buffers to a recorded anchor.\n"
        "    fn rewind_to(&mut self, anchor: crate::agent::turn_control::TranscriptAnchor) {\n"
        "        self.history.truncate(anchor.history_len);\n"
        "        if let Some(ref mut canonical) = self.canonical {\n"
        "            canonical.truncate(anchor.canonical_len);\n"
        "        }\n"
        "        self.synced = self.canonical.as_ref().map_or(0, |c| c.len());\n"
        "    }\n"
        "\n"
        "    /// Advance the cursor past all current canonical messages.  Call after\n"
        "    /// external code modifies both buffers (e.g. `drive_live_sop_actions`).\n",
    )

    # Where the transcript stands now, so a checkpoint taken during this round
    # records the position the round starts from.
    replace_required(
        turn,
        "        let PreparedToolCalls {\n"
        "            mut ordered_results,\n"
        "            mut executable_indices,\n"
        "            mut executable_calls,\n"
        "            mut stream_calls,\n"
        "        } = prepare_tool_calls(\n",
        "        publish_transcript_anchor(&control, &turn_state);\n"
        "        let PreparedToolCalls {\n"
        "            mut ordered_results,\n"
        "            mut executable_indices,\n"
        "            mut executable_calls,\n"
        "            mut stream_calls,\n"
        "        } = prepare_tool_calls(\n",
    )

    # What the tools asked for while the previous round ran. It is applied
    # here, before the iteration assembles its tool set, so a turn that enters
    # plan mode stops offering the write tools at once.
    replace_required(
        turn,
        "    for iteration in 0..max_iterations {\n"
        "        let mut has_new_user_input = iteration == 0;\n",
        "    for iteration in 0..max_iterations {\n"
        "        apply_turn_requests(&control, &mut turn_state);\n"
        "        let mut has_new_user_input = iteration == 0;\n",
    )

    # The two helpers the loop uses. They live beside the turn state they
    # act on.
    replace_required(
        turn,
        "        self.synced = self.canonical.as_ref().map_or(0, |c| c.len());\n"
        "    }\n"
        "}\n",
        "        self.synced = self.canonical.as_ref().map_or(0, |c| c.len());\n"
        "    }\n"
        "}\n"
        "\n"
        "/// Apply what the tools asked for during the previous round.\n"
        "///\n"
        "/// A mode change takes effect at once. A rewind restores the transcript\n"
        "/// to the checkpoint it names, in both buffers, so the messages the\n"
        "/// rewind discards are not replayed into the durable history after the\n"
        "/// loop ends.\n"
        "fn apply_turn_requests(\n"
        "    control: &crate::agent::turn_control::TurnControlHandle,\n"
        "    turn_state: &mut TurnState<'_>,\n"
        ") {\n"
        "    let requests = {\n"
        "        let mut state = match control.lock() {\n"
        "            Ok(state) => state,\n"
        "            Err(poisoned) => poisoned.into_inner(),\n"
        "        };\n"
        "        state.take_requests()\n"
        "    };\n"
        "    for request in requests {\n"
        "        match request {\n"
        "            crate::agent::turn_control::TurnRequest::SetMode(mode) => {\n"
        "                let mut state = match control.lock() {\n"
        "                    Ok(state) => state,\n"
        "                    Err(poisoned) => poisoned.into_inner(),\n"
        "                };\n"
        "                state.set_mode(mode);\n"
        "            }\n"
        "            crate::agent::turn_control::TurnRequest::Rewind { checkpoint_id } => {\n"
        "                let anchor = {\n"
        "                    let state = match control.lock() {\n"
        "                        Ok(state) => state,\n"
        "                        Err(poisoned) => poisoned.into_inner(),\n"
        "                    };\n"
        "                    state.checkpoint(&checkpoint_id).map(|entry| entry.anchor)\n"
        "                };\n"
        "                if let Some(anchor) = anchor {\n"
        "                    turn_state.rewind_to(anchor);\n"
        "                }\n"
        "            }\n"
        "        }\n"
        "    }\n"
        "}\n"
        "\n"
        "/// Publish where the transcript stands, so a checkpoint taken during\n"
        "/// this round records the position the round starts from.\n"
        "fn publish_transcript_anchor(\n"
        "    control: &crate::agent::turn_control::TurnControlHandle,\n"
        "    turn_state: &TurnState<'_>,\n"
        ") {\n"
        "    let mut state = match control.lock() {\n"
        "        Ok(state) => state,\n"
        "        Err(poisoned) => poisoned.into_inner(),\n"
        "    };\n"
        "    state.publish_anchor(turn_state.anchor());\n"
        "}\n",
    )

    # ── plan mode: what the model may call ──────────────────────────────
    # The write and execute tools are not offered while the turn is in plan
    # mode. They stay in the known set, so a call to one is refused with the
    # plan-mode message instead of an "unknown tool" error.
    replace_required(
        specs,
        "    // >>> herdr-prompt-cache: deterministic tool ordering\n",
        "    // Plan mode: the turn may read but not act, so the write and execute\n"
        "    // tools are not offered to the model. A canonical family offers both\n"
        "    // read and write operations, so it stays offered and the call-time\n"
        "    // check judges the operation the action names.\n"
        "    if crate::agent::turn_control::current_mode()\n"
        "        == crate::agent::turn_control::AgentMode::Plan\n"
        "    {\n"
        "        for spec in &tool_specs {\n"
        "            let is_family = crate::tools::families::family_by_name(&spec.name).is_some();\n"
        "            if !is_family\n"
        "                && !crate::agent::turn_control::allowed_in_mode(\n"
        "                    crate::agent::turn_control::AgentMode::Plan,\n"
        "                    &spec.name,\n"
        "                    None,\n"
        "                )\n"
        "            {\n"
        "                hidden_builtin_names.insert(spec.name.clone());\n"
        "            }\n"
        "        }\n"
        "    }\n"
        "    // >>> herdr-prompt-cache: deterministic tool ordering\n",
    )

    # Plan mode refuses a call that would change the machine, with a message
    # that says what to do instead. The refusal happens before the approval
    # gate: a call that plan mode withholds is not one the operator should be
    # asked about.
    replace_required(
        prep,
        "        crate::agent::set_runtime_approved_arg(&tool_name, &mut tool_args, false);\n",
        "        crate::agent::set_runtime_approved_arg(&tool_name, &mut tool_args, false);\n"
        "\n"
        "        // Plan mode: the turn may read but not act. A call that would\n"
        "        // change the machine is refused here, with a message that says\n"
        "        // what to do instead, so the model does not have to guess.\n"
        "        let mode = crate::agent::turn_control::current_mode();\n"
        "        if mode == crate::agent::turn_control::AgentMode::Plan\n"
        "            && !crate::agent::turn_control::allowed_in_mode(\n"
        "                mode,\n"
        "                &tool_name,\n"
        "                tool_args.get(\"action\").and_then(|value| value.as_str()),\n"
        "            )\n"
        "        {\n"
        "            let refusal = crate::agent::turn_control::plan_mode_refusal(&tool_name);\n"
        "            let outcome = crate::agent::tool_execution::ToolExecutionOutcome {\n"
        "                output: format!(\"Error: {refusal}\"),\n"
        "                attachments: Vec::new(),\n"
        "                output_data: None,\n"
        "                success: false,\n"
        "                error_reason: Some(refusal),\n"
        "                duration: std::time::Duration::ZERO,\n"
        "                receipt: None,\n"
        "            };\n"
        "            // The before phase ran but this call will never execute:\n"
        "            // its only terminal lifecycle operation is abandonment.\n"
        "            abandon_prepared_context(ctx, &hook_context, &tool_name).await;\n"
        "            if let Some(tx) = ctx.event_tx {\n"
        "                emit_tool_call_pair(tx, call, &outcome).await;\n"
        "            }\n"
        "            ordered_results[idx] =\n"
        "                Some((tool_name.clone(), call.tool_call_id.clone(), outcome));\n"
        "            continue;\n"
        "        }\n",
    )

    # ── the tools ───────────────────────────────────────────────────────
    replace_required(
        runtime_tools,
        "pub mod cron_update;\n",
        "pub mod checkpoint;\npub mod cron_update;\n",
    )
    replace_required(
        runtime_tools,
        "pub use cron_update::CronUpdateTool;\n",
        "pub use checkpoint::CheckpointTool;\npub use cron_update::CronUpdateTool;\n",
    )
    replace_required(
        runtime_tools,
        "pub mod model_switch;\n",
        "pub mod model_switch;\npub mod plan_execute;\npub mod plan_mode;\n",
    )
    replace_required(
        runtime_tools,
        "pub use model_switch::ModelSwitchTool;\n",
        "pub use model_switch::ModelSwitchTool;\n"
        "pub use plan_execute::PlanExecuteTool;\n"
        "pub use plan_mode::PlanModeTool;\n",
    )
    replace_required(
        runtime_tools,
        "pub mod read_skill;\n",
        "pub mod read_skill;\npub mod rewind;\n",
    )
    replace_required(
        runtime_tools,
        "pub use read_skill::ReadSkillTool;\n",
        "pub use read_skill::ReadSkillTool;\npub use rewind::RewindTool;\n",
    )
    replace_required(
        runtime_tools,
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n',
        '    register_tool!("calculator", Arc::new(CalculatorTool::new()));\n'
        "    // The turn's control channel, as tools. `checkpoint` and `rewind`\n"
        "    // mark and return to a point in the conversation; `plan_mode` puts\n"
        "    // the turn in plan mode, and `plan_execute` asks the operator to\n"
        "    // let it act again.\n"
        '    register_tool!(\n'
        '        "checkpoint",\n'
        "        Arc::new(crate::tools::checkpoint::CheckpointTool::new())\n"
        "    );\n"
        '    register_tool!(\n'
        '        "rewind",\n'
        "        Arc::new(crate::tools::rewind::RewindTool::new())\n"
        "    );\n"
        '    register_tool!(\n'
        '        "plan_mode",\n'
        "        Arc::new(crate::tools::plan_mode::PlanModeTool::new())\n"
        "    );\n"
        '    register_tool!(\n'
        '        "plan_execute",\n'
        "        Arc::new(crate::tools::plan_execute::PlanExecuteTool::new())\n"
        "    );\n",
    )

    replace_required(
        attribution,
        "use crate::tools::verifiable_intent::VerifiableIntentTool;\n",
        "use crate::tools::checkpoint::CheckpointTool;\n"
        "use crate::tools::plan_execute::PlanExecuteTool;\n"
        "use crate::tools::plan_mode::PlanModeTool;\n"
        "use crate::tools::rewind::RewindTool;\n"
        "use crate::tools::verifiable_intent::VerifiableIntentTool;\n",
    )
    replace_required(
        attribution,
        "tool_attribution!(VerifiableIntentTool, ToolKind::Plugin);\n",
        "tool_attribution!(CheckpointTool, ToolKind::Plugin);\n"
        "tool_attribution!(PlanExecuteTool, ToolKind::Plugin);\n"
        "tool_attribution!(PlanModeTool, ToolKind::Plugin);\n"
        "tool_attribution!(RewindTool, ToolKind::Plugin);\n"
        "tool_attribution!(VerifiableIntentTool, ToolKind::Plugin);\n",
    )

    # ── the inventory rows ───────────────────────────────────────────────
    inventory = source / "crates/zeroclaw-tools/src/inventory.rs"
    replace_required(
        inventory,
        "    // Tier 3: optional. Utilities.\n",
        "    // Tier 2: host-coupled. The turn's control channel.\n"
        + "".join(
            "    BuiltinToolSpec {\n"
            f'        name: "{name}",\n'
            "        tier: ToolTier::Host,\n"
            "        compilation: BuiltinToolCompilation::Always,\n"
            "    },\n"
            for name in ("checkpoint", "rewind", "plan_mode", "plan_execute")
        )
        + "    // Tier 3: optional. Utilities.\n",
    )

    # ── the inventory docs page ──────────────────────────────────────────
    replace_required(
        source / "docs/book/src/developing/tool-inventory.md",
        "| Runtime-defined | `security_ops` | Lives in the runtime crate and uses its security playbook and vulnerability modules. |\n",
        "| Runtime-defined | `security_ops` | Lives in the runtime crate and uses its security playbook and vulnerability modules. |\n"
        "| Turn control | `checkpoint`, `rewind`, `plan_mode`, `plan_execute` | `checkpoint` marks a point in the conversation and `rewind` returns to it, through the turn loop's control channel; `plan_mode` puts the turn in plan mode, where only the read-tier tools run, and `plan_execute` asks the operator to let it act again. |\n",
    )

    # ── the builtin-selection fixture ────────────────────────────────────
    fixture = source / "crates/zeroclaw-runtime/tests/builtin_selection_boundary.rs"
    text = fixture.read_text()
    anchor = '        "calculator",\n'
    if text.count(anchor) != 1:
        raise RuntimeError(f"Upstream overlay drift: expected one {anchor!r} in {fixture}")
    additions = "".join(
        f'        "{name}",\n'
        for name in ("checkpoint", "rewind", "plan_mode", "plan_execute")
    )
    fixture.write_text(text.replace(anchor, anchor + additions))

def project_enact_plugin_hooks(source):
    """Let a WebAssembly plugin serve as a hook.

    A declarative hook answers by running a shell command; a plugin hook
    answers in process. Both are consulted by the same hook runner, at the
    same points, so a plugin hook composes with the command hooks rather than
    replacing them.

    The plugin declares the `hook` capability and exports the `hook-plugin`
    world. The host binds it to a `HookHandler`, so the turn's runner can ask
    it before a tool call, after a tool call, and before the prompt is built.

    A hook that fails does not get to decide: a plugin that traps, misses its
    deadline, or answers with something the host cannot read leaves the turn
    as if the hook had not been asked.
    """
    plugins = source / "crates/zeroclaw-plugins/src/lib.rs"
    component = source / "crates/zeroclaw-plugins/src/component.rs"
    logging = source / "crates/zeroclaw-plugins/src/component_logging.rs"
    host = source / "crates/zeroclaw-plugins/src/host.rs"
    validate = source / "crates/zeroclaw-plugins/src/validate.rs"
    runtime = source / "crates/zeroclaw-runtime/src/plugin_runtime.rs"

    # ── the adapter ─────────────────────────────────────────────────────
    overlay = ROOT / "overlays/enact/native/plugins/wasm_hook.rs"
    target = source / "crates/zeroclaw-plugins/src/wasm_hook.rs"
    if target.exists():
        raise RuntimeError(
            f"Upstream overlay drift: the hook adapter already exists at {target}"
        )
    shutil.copy2(overlay, target)

    # ── the capability ──────────────────────────────────────────────────
    replace_required(
        plugins,
        "    /// Provides one or more agentskills.io-format skills under `skills/`\n"
        "    Skill,\n"
        "}\n",
        "    /// Provides one or more agentskills.io-format skills under `skills/`\n"
        "    Skill,\n"
        "    /// Provides a hook: a callback the turn consults while it runs\n"
        "    Hook,\n"
        "}\n",
    )
    replace_required(
        plugins,
        "#[cfg(feature = \"plugins-wasmtime\")]\n"
        "pub mod wasm_channel;\n",
        "#[cfg(feature = \"plugins-wasmtime\")]\n"
        "pub mod wasm_channel;\n"
        "#[cfg(feature = \"plugins-wasmtime\")]\n"
        "pub mod wasm_hook;\n",
    )
    replace_required(
        runtime,
        '        PluginCapability::Channel => "channel",\n',
        '        PluginCapability::Channel => "channel",\n'
        '        PluginCapability::Hook => "hook",\n',
    )

    # ── the world ───────────────────────────────────────────────────────
    replace_required(
        component,
        "    pub mod memory {\n"
        "        wasmtime::component::bindgen!({\n"
        '            world: "memory-plugin",\n'
        '            path: "wit/v0",\n'
        "            imports: { default: async },\n"
        "            exports: { default: async },\n"
        "        });\n"
        "    }\n"
        "}\n",
        "    pub mod memory {\n"
        "        wasmtime::component::bindgen!({\n"
        '            world: "memory-plugin",\n'
        '            path: "wit/v0",\n'
        "            imports: { default: async },\n"
        "            exports: { default: async },\n"
        "        });\n"
        "    }\n"
        "    pub mod hook {\n"
        "        wasmtime::component::bindgen!({\n"
        '            world: "hook-plugin",\n'
        '            path: "wit/v0",\n'
        "            imports: { default: async },\n"
        "            exports: { default: async },\n"
        "        });\n"
        "    }\n"
        "}\n",
    )
    # The hook world imports logging, so the logging host has to be linked
    # for it as well.
    replace_required(
        logging,
        "impl_host!(memory);\n",
        "impl_host!(memory);\nimpl_host!(hook);\n",
    )

    # ── the host's view of a hook plugin ────────────────────────────────
    replace_required(
        host,
        "    /// Get skill-capable plugins.\n"
        "    pub fn skill_plugins(&self) -> Vec<&PluginManifest> {\n",
        "    /// Get hook-capable plugins.\n"
        "    pub fn hook_plugins(&self) -> Vec<&PluginManifest> {\n"
        "        self.loaded\n"
        "            .values()\n"
        "            .filter(|p| p.manifest.capabilities.contains(&PluginCapability::Hook))\n"
        "            .map(|p| &p.manifest)\n"
        "            .collect()\n"
        "    }\n"
        "\n"
        "    /// The admitted components of the hook-capable plugins.\n"
        "    pub fn hook_plugin_details(&self) -> Vec<(&PluginManifest, &AdmittedComponent)> {\n"
        "        self.executable_plugin_details(PluginCapability::Hook)\n"
        "    }\n"
        "\n"
        "    /// Get skill-capable plugins.\n"
        "    pub fn skill_plugins(&self) -> Vec<&PluginManifest> {\n",
    )

    # ── the install-time load check ─────────────────────────────────────
    replace_required(
        validate,
        "            // Memory and other capabilities are covered by the compile check\n"
        "            // above; a dedicated instantiate world for them is a follow-up.\n"
        "            _ => {}\n",
        "            PluginCapability::Hook => {\n"
        "                let scope = PluginInstanceScope::for_package_binding(\n"
        "                    manifest,\n"
        "                    PluginCapability::Hook,\n"
        "                    manifest.permissions.iter().copied(),\n"
        "                )?;\n"
        "                crate::wasm_hook::verify_hook_loads(component, &scope, &services, limits)\n"
        "                    .await?;\n"
        "            }\n"
        "            // Memory and other capabilities are covered by the compile check\n"
        "            // above; a dedicated instantiate world for them is a follow-up.\n"
        "            _ => {}\n",
    )

    # ── the documentation ───────────────────────────────────────────────
    replace_required(
        source / "docs/book/src/plugins/index.md",
        "- **Capabilities**: what the plugin *is*. One or more of `tool`, `channel`,\n"
        "  `memory`, `observer`, `skill` (the `PluginCapability` enum in\n",
        "- **Capabilities**: what the plugin *is*. One or more of `tool`, `channel`,\n"
        "  `memory`, `observer`, `skill`, `hook` (the `PluginCapability` enum in\n",
    )
    replace_required(
        source / "docs/book/src/plugins/index.md",
        "| `observer` | none | `PluginCapability::Observer` is reserved; no WIT world or adapter exists yet |\n",
        "| `observer` | none | `PluginCapability::Observer` is reserved; no WIT world or adapter exists yet |\n"
        "| `hook` | `WasmHook`, implements the `HookHandler` trait | Registered with the same hook runner the declarative command hooks use, so a plugin hook composes with them rather than replacing them |\n",
    )

    # ── the interface definition ────────────────────────────────────────
    install_enact_hook_wit(source)

def project_enact_builtin_selection_fixture(source):
    """Keep the builtin-selection fixture in step with the shipped registry.

    The fixture is a snapshot of the tools a full chat configuration
    assembles. Every tool the overlay adds — and every canonical family the
    consolidation adds — belongs in it.
    """
    fixture = source / "crates/zeroclaw-runtime/tests/builtin_selection_boundary.rs"
    text = fixture.read_text()
    anchor = '        "calculator",\n'
    if text.count(anchor) != 1:
        raise RuntimeError(f"Upstream overlay drift: expected one {anchor!r} in {fixture}")
    additions = "".join(f'        "{name}",\n' for name in ("ast_edit", "ast_grep", "callgraph", "compose", "expand", "graph", "radar", "cron", "learn", "memory", "model", "sessions", "skill_manage", "skill_view", "skills_list", "wait"))
    fixture.write_text(text.replace(anchor, anchor + additions))


def project_enact_branding_consistency(source):
    """Keep the branded tree's own expectations in step with its branding.

    The branding overlay renames the product, the command, the built-in skill
    and the environment variables. Every upstream expectation that names one
    of them has to follow, or the tree ships a test suite that fails against
    the product it actually builds.

    Two of the fixes here are more than renames:

    * `Config::agent_workspace_dir` anchors the agent's workspace to the
      install root rather than to `$HOME`. A config in its own directory has
      to own its agents' workspaces, and a test that builds a config in a
      temporary directory has to get its workspace there too — not in the
      operator's real home, where the next test run finds it.
    * The locale catalogues name the environment variable the plugin
      registry reads. The Rust side was renamed and the message was not, so
      the operator was told to set a variable the binary ignores.
    """
    brand = manifest()

    # ── the workspace shape the branded tree uses ───────────────────────
    browse = source / "crates/zeroclaw-runtime/src/browse.rs"
    browse_text = browse.read_text()
    old_shape = "agents/alpha/workspace"
    new_shape = "workspace/agents/alpha"
    if old_shape not in browse_text:
        raise RuntimeError(f"Upstream overlay drift: expected {old_shape!r} in {browse}")
    browse.write_text(browse_text.replace(old_shape, new_shape))

    # ── the built-in skill's name ───────────────────────────────────────
    # The embedded asset keeps its upstream path; only the skill's name
    # changed, and every expectation that names it has to follow.
    skills = source / "crates/zeroclaw-runtime/src/skills/mod.rs"
    skills_text = skills.read_text()
    include = 'include_str!("assets/zeroclaw-docs/SKILL.md")'
    if skills_text.count(include) != 1:
        raise RuntimeError(f"Upstream overlay drift: expected one {include!r} in {skills}")
    head, _, tail = skills_text.partition(include)
    tail = tail.replace("zeroclaw-docs", "enact-agent-docs")
    skills.write_text(head + include + tail)

    for relative in (
        "crates/zeroclaw-runtime/src/skills/service.rs",
        "crates/zeroclaw-runtime/src/tools/read_skill.rs",
        "crates/zeroclaw-runtime/src/tools/mod.rs",
    ):
        path = source / relative
        text = path.read_text()
        if "zeroclaw-docs" not in text:
            raise RuntimeError(f"Upstream overlay drift: expected the old skill name in {path}")
        text = text.replace("zeroclaw-docs", "enact-agent-docs")
        # The resolver lists the available skills in sorted order, so the
        # renamed skill moves to the front of the list.
        text = text.replace(
            "Available skills: weather, enact-agent-docs",
            "Available skills: enact-agent-docs, weather",
        )
        path.write_text(text)

    # ── the product literal in the expectations ──────────────────────────
    for relative in (
        "crates/zeroclaw-runtime/src/skills/mod.rs",
        "crates/zeroclaw-runtime/src/tools/read_skill.rs",
    ):
        path = source / relative
        text = path.read_text()
        text = text.replace(
            "# ZeroClaw self-knowledge and operation",
            "# Enact Agent self-knowledge and operation",
        )
        path.write_text(text)

    skills = source / "crates/zeroclaw-runtime/src/skills/mod.rs"
    text = skills.read_text()
    text = text.replace(
        "current ZeroClaw installation",
        "active Enact Agent installation",
    )
    text = text.replace("not audited by ZeroClaw", "not audited by Enact Agent")
    text = text.replace("zeroclaw skills audit", "enact-agent skills audit")
    text = text.replace("# ZeroClaw Skills", "# Enact Agent Skills")
    skills.write_text(text)

    # ── the refusal messages the locale drives ──────────────────────────
    # Only the test module asserts the product name; the production doc
    # comments describe upstream behaviour and stay as they are.
    authority = source / "crates/zeroclaw-runtime/src/live_config_authority.rs"
    text = authority.read_text()
    marker = "#[cfg(test)]\nmod tests"
    if text.count(marker) != 1:
        raise RuntimeError(f"Upstream overlay drift: expected one test module in {authority}")
    head, _, tail = text.partition(marker)
    authority.write_text(head + marker + tail.replace("ZeroClaw", brand["name"]))

    # ── the environment variable the messages name ──────────────────────
    # The Rust side reads `ENACT_AGENT_<path>`; a message that still says
    # `ZEROCLAW_<path>` tells the operator to set a variable the binary
    # ignores. Every catalogue follows the code.
    for catalogue in source.rglob("*.ftl"):
        text = catalogue.read_text()
        if "ZEROCLAW_" in text:
            catalogue.write_text(text.replace("ZEROCLAW_", "ENACT_AGENT_"))

    # ── the transcript fixture the branded renderer changed ─────────────
    # The overlay adds a `success` field to a rendered tool result, so the
    # shared fixture carries it too.
    for relative in (
        "crates/zeroclaw-runtime/tests/fixtures/single-tool-rounds.json",
        "apps/zerocode/tests/fixtures/single-tool-rounds.json",
    ):
        path = source / relative
        if not path.is_file():
            raise RuntimeError(f"Upstream overlay drift: missing the transcript fixture {path}")
        text = path.read_text()
        for result in ("result-1", "result-2"):
            old = f'"raw_output": "{result}"'
            new = f'"raw_output": "{result}", "success": true'
            if old not in text:
                raise RuntimeError(
                    f"Upstream overlay drift: expected {old!r} in {path}"
                )
            text = text.replace(old, new)
        path.write_text(text)

    # ── the desktop fixtures read the environment the runtime sets ──────
    for relative in (
        "crates/zeroclaw-runtime/tests/fixtures/desktop-generation-one.sh",
        "crates/zeroclaw-runtime/tests/fixtures/desktop-generation-two.sh",
    ):
        path = source / relative
        if not path.is_file():
            raise RuntimeError(f"Upstream overlay drift: missing the desktop fixture {path}")
        text = path.read_text()
        text = text.replace("ZEROCLAW_DESKTOP_", "ENACT_AGENT_DESKTOP_")
        path.write_text(text)


def project_enact_config_dir_branding(source):
    """Keep the config crate's own expectations in step with the branded home.

    The branded product keeps its configuration in `$ENACT_HOME/agent`, or
    `~/.enact/agent` when no home is named, and the data directory beside it.
    The upstream expectations still name `~/.zeroclaw`, so the branded tree
    fails its own suite: the fixtures build a config in one place and the
    loader looks in another.

    The container layout is deliberately left alone. A container image keeps
    its install beside the data directory, and that install is found by the
    legacy `.zeroclaw` name, so the branded tree has to keep recognising it.

    One failure here is not a branding problem at all: on a case-insensitive
    filesystem `nul` and `NUL` are one filename, so the policy fixture cannot
    create both aliases. The test skips the folded name instead of failing.
    """
    config = source / "crates/zeroclaw-config/src/schema.rs"
    native = source / "crates/zeroclaw-config/src/platform/native.rs"
    policy = source / "crates/zeroclaw-config/src/policy.rs"
    recovery = source / "crates/zeroclaw-config/tests/legacy_plugin_recovery.rs"

    # ── the storage path names the branded home ─────────────────────────
    replace_required(
        native,
        '    fn native_storage_path_contains_zeroclaw() {\n'
        "        let path = NativeRuntime::new().storage_path();\n"
        '        assert!(path.to_string_lossy().contains("zeroclaw"));\n'
        "    }\n",
        "    fn native_storage_path_contains_enact() {\n"
        "        let path = NativeRuntime::new().storage_path();\n"
        '        assert!(path.to_string_lossy().contains("enact"));\n'
        "    }\n",
    )

    # ── the fallbacks agree with the branded default ────────────────────
    replace_required(
        config,
        '            home.join(".zeroclaw")\n        });\n',
        '            home.join(".enact").join("agent")\n        });\n',
    )
    replace_required(
        config,
        '        Err(_) => format!("~/.zeroclaw/{relative}"),\n',
        '        Err(_) => format!("~/.enact/agent/{relative}"),\n',
    )

    # ── the fixtures build the config where the loader looks for it ─────
    replace_required(
        config,
        '                "default ~/.zeroclaw",\n'
        "                default_home.clone(),\n"
        "                None,\n"
        "                None,\n"
        '                Some(default_home.join(".zeroclaw/config.toml")),\n'
        '                default_home.join(".zeroclaw"),\n'
        '                default_home.join(".zeroclaw/data"),\n',
        '                "default ~/.enact/agent",\n'
        "                default_home.clone(),\n"
        "                None,\n"
        "                None,\n"
        '                Some(default_home.join(".enact/agent/config.toml")),\n'
        '                default_home.join(".enact/agent"),\n'
        '                default_home.join(".enact/agent/data"),\n',
    )
    replace_required(
        config,
        '        for layout in ["default ~/.zeroclaw", "container", "explicit --config-dir"] {\n',
        '        for layout in ["default ~/.enact/agent", "container", "explicit --config-dir"] {\n',
    )
    replace_required(
        config,
        '                _ => (home.join(".zeroclaw"), None, None),\n',
        '                _ => (home.join(".enact/agent"), None, None),\n',
    )
    replace_required(
        config,
        '            classify_runtime_config_kind(&fake_home.join(".zeroclaw").join("config.toml")).await,\n',
        '            classify_runtime_config_kind(\n'
        '                &fake_home.join(".enact/agent").join("config.toml"),\n'
        "            )\n"
        "            .await,\n",
    )
    replace_required(
        config,
        '        let config_dir = temp_home.join(".zeroclaw");\n',
        '        let config_dir = temp_home.join(".enact/agent");\n',
    )

    # The container layout keeps the install beside the data directory, which
    # is where the loader looks for it; only the default layout moves.
    replace_required(
        recovery,
        "        } else {\n"
        '            home.join(".zeroclaw")\n'
        "        };\n",
        "        } else if layout == \"container\" {\n"
        '            home.join(".zeroclaw")\n'
        "        } else {\n"
        '            home.join(".enact/agent")\n'
        "        };\n",
    )

    # ── a case-insensitive filesystem folds `nul` onto `NUL` ─────────────
    replace_required(
        policy,
        "            let alias = workspace.join(name);\n"
        "            symlink(target, &alias).unwrap();\n",
        "            let alias = workspace.join(name);\n"
        "            // A case-insensitive filesystem folds `nul` onto `NUL`, so\n"
        "            // the second spelling is the first one and already holds\n"
        "            // the assertions below.\n"
        "            if let Err(error) = symlink(target, &alias) {\n"
        "                assert_eq!(\n"
        "                    error.kind(),\n"
        "                    std::io::ErrorKind::AlreadyExists,\n"
        "                    \"{name:?} must be creatable: {error}\"\n"
        "                );\n"
        "                continue;\n"
        "            }\n",
    )


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
            # A tracked entry can be a symlink to a directory (`crates/
            # zeroclaw-plugins/wit` points at the workspace root `wit/`). Copy
            # the tree it points at: `is_file()` is false for a directory, so a
            # plain file copy would silently drop the directory and the crate
            # would fail to compile against a missing WIT world.
            if source.is_symlink():
                target = source.resolve()
                if target.is_dir():
                    dest = SOURCE / name
                    shutil.copytree(target, dest, dirs_exist_ok=True, symlinks=True)
                    continue
                if not target.is_file():
                    continue
                source = target
            if not source.is_file():
                continue
            dest = SOURCE / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest)
    # These ignored outputs are prerequisites produced by the normal web build.
    if not committed and (ROOT / "web/dist/index.html").is_file():
        shutil.copytree(ROOT / "web/dist", SOURCE / "web/dist", dirs_exist_ok=True)

    project_enact_builtin_docs_skill(SOURCE)
    project_enact_mcp_server(SOURCE)
    project_enact_declarative_hooks(SOURCE)
    project_enact_trust_split(SOURCE)
    project_enact_hooks_docs(SOURCE)
    project_enact_context_tools(SOURCE)
    project_enact_tool_families(SOURCE)
    project_enact_coordination(SOURCE)
    project_enact_turn_control(SOURCE)
    project_enact_plugin_hooks(SOURCE)
    project_enact_branding_consistency(SOURCE)
    project_enact_builtin_selection_fixture(SOURCE)
    project_enact_agent_capabilities(SOURCE)
    project_enact_config_dir_branding(SOURCE)

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
    project_personality_templates(SOURCE)
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
    update_signature = (
        "pub async fn run(target_version: Option<&str>, force: bool, standard: bool) -> Result<()> {"
    )
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
    project_workspace_owned_agent_paths(SOURCE)
    project_session_tools(SOURCE)
    project_tool_result_status(SOURCE)
    # The advertised tool set is derived, so it is checked against the tree
    # that actually ships, once every projection has added its inventory rows.
    check_enact_tool_inventory(SOURCE)
    run(cargo, "fmt", "--all", cwd=SOURCE)
    print(f"Prepared branded source: {SOURCE}")
    return brand


def verify_builtin_capabilities(cargo, source, env, target=None, features=NATIVE_FEATURES, committed=False):
    """Execute the Always/External capability contract against the selected graph."""
    args = [cargo, "test", "--locked", *( ["--offline"] if committed else []), "--release", "--no-default-features",
            "--features", ",".join(features), "--test", "enact_builtin_capabilities"]
    if target is not None:
        args.extend(["--target", target])
    run(*args, cwd=source, env=env)


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
    args = ["cargo", "build", "--locked", "--bin", "zeroclaw", "--features", ",".join(DISTRIBUTION_FEATURES)]
    if not debug:
        args.append("--release")
    verify_builtin_capabilities("cargo", SOURCE, env, features=DISTRIBUTION_FEATURES)
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
    verify_builtin_capabilities(cargo, SOURCE, env, target=target, committed=committed)
    run(cargo, "build", "--locked", *(["--offline"] if committed else []), "--release", "--no-default-features",
        "--features", ",".join(NATIVE_FEATURES), "--target", target, "--bin", "zeroclaw", cwd=SOURCE, env=env)
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
        "build": {"defaultFeatures": False, "features": list(NATIVE_FEATURES),
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
