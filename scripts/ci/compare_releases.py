#!/usr/bin/env python3
"""Compare the tool surface of two published Enact Agent releases.

Both releases publish a `manifest.json` inside their archive that records the
binary's identity and provenance. The tool *surface* itself lives in the
generated contract: `overlays/enact/contract/tool-contract.json` for a build
of this tree, and the registry a released binary was built from for a build
of an older tree. A release published before the tool-surface convergence
carries no contract at all, and the comparison reports that absence rather
than silently reporting "no change".

Usage:
    compare_releases.py OLD_MANIFEST NEW_MANIFEST
    compare_releases.py --old-tag enact-agent-v0.9.0-enact.1 \
                         --new-tag enact-agent-v0.9.0-enact.2

The `--old-tag` / `--new-tag` form downloads the archives from the fork's
GitHub Releases and extracts the manifests. The contract for the new build
is read from this repository, and the old build's contract is reported as
absent unless a path to it is given.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path
from typing import Any

REPO = "adl-tan-theta/enact-agent"
RELEASE_BASE = f"https://github.com/{REPO}/releases/download"
REPO_ROOT = Path(__file__).resolve().parents[2]
CONTRACT_PATH = REPO_ROOT / "overlays" / "enact" / "contract" / "tool-contract.json"


def fetch_manifest(tag: str, target: str, workdir: Path) -> dict[str, Any]:
    """Download a release archive and return the manifest.json inside it."""
    archive_name = f"{tag}-{target}.tar.gz"
    url = f"{RELEASE_BASE}/{tag}/{archive_name}"
    archive = workdir / archive_name
    subprocess.run(
        ["curl", "-fsSL", "-o", str(archive), url],
        check=True,
    )
    with tarfile.open(archive, "r:gz") as stream:
        for member in stream.getmembers():
            if member.name.endswith("/manifest.json"):
                extracted = stream.extractfile(member)
                if extracted is None:
                    break
                return json.loads(extracted.read().decode())
    raise SystemExit(f"no manifest.json inside {archive_name}")


def load_manifest(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text())


def families_of(contract: dict[str, Any] | None) -> dict[str, Any]:
    """The family contract, empty when the build carries none.

    A release built before the tool-surface convergence has no contract at
    all. That absence is the most important difference the comparison
    reports, so it is represented as an empty mapping rather than as a
    missing key.
    """
    if not contract:
        return {}
    return contract.get("families", {}) or {}


def canonical_of(contract: dict[str, Any] | None) -> list[str]:
    if not contract:
        return []
    return list(contract.get("canonical_tools", []) or [])


def legacy_of(contract: dict[str, Any] | None) -> list[str]:
    if not contract:
        return []
    return list(contract.get("legacy_members", []) or [])


def load_contract(path: Path | None) -> dict[str, Any] | None:
    """Read a generated contract, or None when there is none to read."""
    if path is None:
        return None
    if not path.is_file():
        return None
    return json.loads(path.read_text())


def report(
    old: dict[str, Any],
    new: dict[str, Any],
    old_contract: dict[str, Any] | None = None,
    new_contract: dict[str, Any] | None = None,
) -> int:
    old_bin = old.get("binary", {})
    new_bin = new.get("binary", {})

    print("Binary")
    print(f"  version   {old_bin.get('version', '?')}  ->  {new_bin.get('version', '?')}")
    print(f"  size      {old_bin.get('sizeBytes', '?')}  ->  {new_bin.get('sizeBytes', '?')} bytes")
    print(f"  sha256    {old_bin.get('sha256', '?')[:16]}...  ->  {new_bin.get('sha256', '?')[:16]}...")
    print(f"  target    {old_bin.get('target', '?')}  ->  {new_bin.get('target', '?')}")

    old_source = old.get("source", {})
    new_source = new.get("source", {})
    print()
    print("Provenance")
    print(f"  upstream  {old_source.get('upstreamCommit', '?')[:12]}  ->  "
          f"{new_source.get('upstreamCommit', '?')[:12]}")
    old_overlay = old_source.get("overlay", {})
    new_overlay = new_source.get("overlay", {})
    print(f"  overlay   {old_overlay.get('sha256', '?')[:16]}...  ->  "
          f"{new_overlay.get('sha256', '?')[:16]}...")

    old_canonical = canonical_of(old_contract)
    new_canonical = canonical_of(new_contract)
    old_legacy = legacy_of(old_contract)
    new_legacy = legacy_of(new_contract)

    print()
    print("Tool surface")
    if old_contract is None and new_contract is None:
        print("  neither build carries a tool contract")
    else:
        if old_contract is None:
            print(f"  contract  absent  ->  present")
        else:
            print(f"  contract  present  ->  {'present' if new_contract else 'absent'}")
        print(f"  canonical {len(old_canonical)}  ->  {len(new_canonical)} tools")
        print(f"  legacy    {len(old_legacy)}  ->  {len(new_legacy)} members")
        gained = sorted(set(new_canonical) - set(old_canonical))
        removed = sorted(set(old_canonical) - set(new_canonical))
        if gained:
            print(f"  added     {', '.join(gained)}")
        if removed:
            print(f"  removed   {', '.join(removed)}")
        dropped_legacy = sorted(set(old_legacy) - set(new_legacy))
        if dropped_legacy:
            print(f"  legacy names withdrawn: {', '.join(dropped_legacy)}")

    old_families = families_of(old_contract)
    new_families = families_of(new_contract)
    old_names = set(old_families)
    new_names = set(new_families)

    print()
    print("Family contract")
    if not old_families and not new_families:
        print("  neither build advertises a family contract")
    else:
        if not old_families:
            print(f"  added     {len(new_names)} families: {', '.join(sorted(new_names))}")
        elif not new_families:
            print(f"  removed   {len(old_names)} families: {', '.join(sorted(old_names))}")
        else:
            added = sorted(new_names - old_names)
            removed = sorted(old_names - new_names)
            kept = sorted(old_names & new_names)
            print(f"  families  {len(old_names)}  ->  {len(new_names)}")
            if added:
                print(f"  added     {', '.join(added)}")
            if removed:
                print(f"  removed   {', '.join(removed)}")
            if kept:
                print(f"  unchanged {', '.join(kept)}")

        changed = []
        for name in sorted(old_names & new_names):
            old_actions = set(old_families[name])
            new_actions = set(new_families[name])
            if old_actions != new_actions:
                changed.append((name, old_actions, new_actions))
        if changed:
            print()
            print("Action changes")
            for name, old_actions, new_actions in changed:
                gained = sorted(new_actions - old_actions)
                lost = sorted(old_actions - new_actions)
                print(f"  {name}")
                if gained:
                    print(f"    + {', '.join(gained)}")
                if lost:
                    print(f"    - {', '.join(lost)}")
        else:
            print()
            print("Action changes")
            print("  none: every family advertises the same actions in both builds")

    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("old_manifest", nargs="?", type=Path, help="path to the old manifest.json")
    parser.add_argument("new_manifest", nargs="?", type=Path, help="path to the new manifest.json")
    parser.add_argument("--old-tag", help="old release tag, e.g. enact-agent-v0.9.0-enact.1")
    parser.add_argument("--new-tag", help="new release tag, e.g. enact-agent-v0.9.0-enact.2")
    parser.add_argument(
        "--target",
        default="aarch64-apple-darwin",
        help="release target whose manifest to read (default: aarch64-apple-darwin)",
    )
    parser.add_argument(
        "--old-contract",
        type=Path,
        help="the old build's tool contract; absent means it carried none",
    )
    parser.add_argument(
        "--new-contract",
        type=Path,
        default=CONTRACT_PATH,
        help=f"the new build's tool contract (default: {CONTRACT_PATH.relative_to(REPO_ROOT)})",
    )
    args = parser.parse_args()

    if args.old_tag and args.new_tag:
        with tempfile.TemporaryDirectory() as name:
            workdir = Path(name)
            old = fetch_manifest(args.old_tag, args.target, workdir)
            new = fetch_manifest(args.new_tag, args.target, workdir)
        return report(
            old,
            new,
            load_contract(args.old_contract),
            load_contract(args.new_contract),
        )

    if args.old_manifest and args.new_manifest:
        return report(
            load_manifest(args.old_manifest),
            load_manifest(args.new_manifest),
            load_contract(args.old_contract),
            load_contract(args.new_contract),
        )

    parser.error(
        "give either two manifest paths, or --old-tag and --new-tag"
    )


if __name__ == "__main__":
    sys.exit(main())
