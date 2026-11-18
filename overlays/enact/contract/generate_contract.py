#!/usr/bin/env python3
"""Generate the Enact tool contract from the overlay's family table.

The contract is the machine-readable vocabulary a host reads: the canonical
tool names the fork advertises, and the actions each consolidated family
exposes. It is generated, never hand-kept, so the two sides cannot drift:
the fork owns the registry, and this file is the fork's statement of what
that registry contains.

The output is deterministic — sorted keys, sorted lists — so the file
diffs cleanly in review and a CI check can tell whether it is stale.

The actions come from the same derivation the runtime uses
(`family_actions` in `native/tools/families.rs`), mirrored here: the family
table names each member and its operations, an operation shared by several
members takes a discriminator derived from the member it came from, and the
result is a bijection over the family's operations.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
FAMILIES_RS = ROOT / "overlays/enact/native/tools/families.rs"
OPERATIONS_CSV = ROOT / "overlays/enact/zeroclaw-tools-operations.csv"
CONTRACT_JSON = ROOT / "overlays/enact/contract/tool-contract.json"


def parse_families(text: str) -> tuple[dict[str, list[str]], dict[str, list[str]]]:
    """The family table: each family's name and its members.

    Only the table itself is read. The file also carries the tests, and a
    later mention of a family's name must not shadow the table's own entry.
    """
    table = _table_block(text, "pub const FAMILIES: &[Family] = &[", "\n];")
    members = {}
    for match in re.finditer(
        r'Member\s*\{\s*tool:\s*"([^"]+)",\s*ops:\s*&\[(.*?)\],\s*'
        r"action_backed:\s*(true|false)\s*,?\s*\}",
        text,
        re.S,
    ):
        members[match.group(1)] = re.findall(r'"([^"]+)"', match.group(2))

    families = {}
    for match in re.finditer(
        r'name:\s*"([^"]+)",\s*\n\s*description:\s*"[^"]*",\s*\n\s*members:\s*&\[(.*?)\]',
        table,
        re.S,
    ):
        families[match.group(1)] = re.findall(r'"([^"]+)"', match.group(2))
    return families, members


def _table_block(text: str, start: str, end: str) -> str:
    """The body of a table declaration, from its opening to its close."""
    begin = text.find(start)
    if begin < 0:
        raise ValueError(f"the table declaration {start!r} is missing")
    finish = text.find(end, begin)
    if finish < 0:
        raise ValueError(f"the table declaration {start!r} is not closed")
    return text[begin:finish]


def family_actions(family: str, families: dict, members: dict) -> list[str]:
    """The actions a family exposes — the runtime's own derivation.

    An operation name shared by several members takes a discriminator
    derived from the member it came from; where that would still collide,
    the whole member name is used.
    """
    pairs: list[tuple[str, str]] = []
    for member in families.get(family, []):
        for op in members.get(member, []):
            pairs.append((member, op))

    seen: set[str] = set()
    shared: set[str] = set()
    for _, op in pairs:
        if op in seen:
            shared.add(op)
        seen.add(op)

    used: set[str] = set()
    for _, op in pairs:
        if op not in shared:
            used.add(op)

    actions: list[str] = []
    for member, op in pairs:
        if op in shared:
            base = member[: -len(op) - 1] if member.endswith(f"_{op}") else member
            candidate = f"{base}_{op}"
            if candidate in used:
                candidate = f"{member}_{op}"
        else:
            candidate = op
        used.add(candidate)
        actions.append(candidate)
    return actions


def canonical_tools() -> list[str]:
    """Every canonical tool name the fork advertises."""
    names = set()
    with OPERATIONS_CSV.open() as handle:
        for index, line in enumerate(handle):
            if index == 0 or not line.strip():
                continue
            names.add(line.split(",", 1)[0].lstrip("#"))
    families, _ = parse_families(FAMILIES_RS.read_text())
    names.update(families)
    return sorted(names)


def legacy_members() -> dict[str, str]:
    """Each legacy member name and the family that replaced it."""
    families, _ = parse_families(FAMILIES_RS.read_text())
    return {
        member: family
        for family, members in families.items()
        for member in members
    }


def build_contract() -> dict:
    families, members = parse_families(FAMILIES_RS.read_text())
    return {
        "families": {
            name: {"actions": family_actions(name, families, members)}
            for name in sorted(families)
        },
        "canonical_tools": canonical_tools(),
        "legacy_members": dict(sorted(legacy_members().items())),
    }


def render(contract: dict) -> str:
    return json.dumps(contract, indent=2, sort_keys=True) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when the committed contract differs from the generated one",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=CONTRACT_JSON,
        help="where to write the contract",
    )
    args = parser.parse_args()

    contract = build_contract()
    rendered = render(contract)

    if args.check:
        if not args.output.is_file():
            print(f"{args.output} is missing; run this script to write it", file=sys.stderr)
            return 1
        committed = args.output.read_text()
        if committed != rendered:
            print(
                f"{args.output} is stale; run `python3 {Path(__file__).name}` to refresh it",
                file=sys.stderr,
            )
            return 1
        print(f"{args.output} is current")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(rendered)
    print(f"Wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
