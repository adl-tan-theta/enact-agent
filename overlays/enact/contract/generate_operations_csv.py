#!/usr/bin/env python3
"""Regenerate the operations inventory with the consolidated family rows.

The inventory is the source-level truth the overlay's family table is
checked against: each row names a tool and one operation it exposes. The
consolidation added the family rows — one canonical tool per family, with
one row per action it exposes — so the inventory says what the fork
advertises, not only what the members used to be called.

The member rows stay: they are the evidence the family table is verified
against. The family rows are added beside them, derived from the same
derivation the runtime uses, so the two cannot disagree.

The output is deterministic — families in table order, actions in the
runtime's order — so the file diffs cleanly in review.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from generate_contract import (  # noqa: E402
    FAMILIES_RS,
    OPERATIONS_CSV,
    family_actions,
    parse_families,
)

HEADER = "tool_name,operation,tier,compilation,source_file,operation_basis,compiled_in_enact"

FAMILY_SOURCE = "overlays/enact/native/tools/families.rs"


def read_rows(text: str) -> list[list[str]]:
    rows = []
    for line in text.splitlines()[1:]:
        if not line.strip():
            continue
        rows.append(line.split(","))
    return rows


def family_rows(families: dict, members: dict) -> list[str]:
    """One row per family action, in the runtime's action order."""
    rows = []
    for family in families:
        for action in family_actions(family, families, members):
            rows.append(
                f"{family},{action},Host,Always,{FAMILY_SOURCE},"
                "consolidated family action,true"
            )
    return rows


def render(rows: list[list[str]], family_row_texts: list[str]) -> str:
    body = [",".join(row) for row in rows]
    # The family rows come first: they are the advertised surface, and the
    # member rows are the evidence behind them.
    return "\n".join([HEADER, *family_row_texts, *body]) + "\n"


def is_family_row(row: list[str]) -> bool:
    """Whether a row is a generated family row.

    The generator reads its own output, so it must recognise the rows it
    wrote: a family row names a family and carries the family source file.
    """
    return len(row) > 4 and row[4] == FAMILY_SOURCE


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when the committed inventory differs from the generated one",
    )
    args = parser.parse_args()

    families, members = parse_families(FAMILIES_RS.read_text())
    rows = read_rows(OPERATIONS_CSV.read_text())
    family_row_texts = family_rows(families, members)

    # The member rows are dropped from the generated file: the inventory is
    # the advertised surface, and the members are no longer advertised. The
    # family rows the generator wrote on a previous run are dropped too, so
    # the generator is idempotent.
    member_names = {member for family in families.values() for member in family}
    kept = [
        row
        for row in rows
        if row[0].lstrip("#") not in member_names and not is_family_row(row)
    ]
    rendered = render(kept, family_row_texts)

    if args.check:
        committed = OPERATIONS_CSV.read_text()
        if committed != rendered:
            print(
                f"{OPERATIONS_CSV} is stale; run `python3 {Path(__file__).name}` to refresh it",
                file=sys.stderr,
            )
            return 1
        print(f"{OPERATIONS_CSV} is current")
        return 0

    OPERATIONS_CSV.write_text(rendered)
    print(f"Wrote {OPERATIONS_CSV}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
