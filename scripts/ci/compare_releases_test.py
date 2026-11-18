#!/usr/bin/env python3
"""Tests the release comparison tool against fixture manifests.

The comparison is the tool that answers "what changed between these two
published releases", so it has to report the case that matters most: a
release built before the tool-surface convergence carries no family
contract at all, and the comparison must show the whole contract arriving
rather than silently reporting no change.
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ci" / "compare_releases.py"

spec = importlib.util.spec_from_file_location("compare_releases", SCRIPT)
assert spec and spec.loader
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)


def manifest(**overrides):
    base = {
        "schemaVersion": 1,
        "binary": {
            "path": "enact-agent",
            "sha256": "a" * 64,
            "sizeBytes": 1234,
            "target": "aarch64-apple-darwin",
            "version": "0.9.0",
        },
        "source": {"upstreamCommit": "b" * 40, "overlay": {"sha256": "c" * 64}},
        "build": {"profile": "release"},
    }
    base.update(overrides)
    return base


class ReleaseComparisonTest(unittest.TestCase):
    def test_a_build_without_a_contract_reports_none(self):
        self.assertEqual(compare.families_of(None), {})
        self.assertEqual(compare.families_of({}), {})
        self.assertEqual(compare.canonical_of(None), [])
        self.assertEqual(compare.legacy_of(None), [])

    def test_families_are_read_from_the_contract(self):
        contract = {
            "canonical_tools": ["cron_add", "cron_run"],
            "legacy_members": ["cron_add"],
            "families": {"cron": {"cron_add": {}, "cron_run": {}}},
        }
        self.assertEqual(compare.families_of(contract), contract["families"])
        self.assertEqual(compare.canonical_of(contract), ["cron_add", "cron_run"])
        self.assertEqual(compare.legacy_of(contract), ["cron_add"])

    def test_the_repository_contract_is_present_and_complete(self):
        # The comparison reads this repository's generated contract as the
        # new build's tool surface. If it is missing or empty the comparison
        # would silently report no change.
        contract = compare.load_contract(compare.CONTRACT_PATH)
        self.assertIsNotNone(contract, "the generated tool contract must exist")
        self.assertEqual(len(compare.families_of(contract)), 10)
        self.assertTrue(compare.canonical_of(contract))

    def test_a_build_with_no_contract_shows_the_whole_contract_arriving(self):
        old = manifest()
        new = manifest()
        new_contract = {
            "canonical_tools": ["cron_add", "sop_list"],
            "legacy_members": ["cron_add"],
            "families": {
                "cron": {"cron_add": {}, "cron_run": {}},
                "sop": {"sop_list": {}},
            },
        }
        report = capture_report(old, new, None, new_contract)
        self.assertIn("added     2 families", report)
        self.assertIn("cron", report)
        self.assertIn("sop", report)
        self.assertIn("absent  ->  present", report)

    def test_action_changes_within_a_family_are_reported(self):
        old_contract = {"families": {"cron": {"cron_add": {}, "cron_run": {}}}}
        new_contract = {"families": {"cron": {"cron_add": {}, "cron_pause": {}}}}
        report = capture_report(manifest(), manifest(), old_contract, new_contract)
        self.assertIn("Action changes", report)
        self.assertIn("+ cron_pause", report)
        self.assertIn("- cron_run", report)

    def test_unchanged_families_are_reported_as_unchanged(self):
        contract = {"families": {"cron": {"cron_add": {}}}}
        report = capture_report(manifest(), manifest(), contract, contract)
        self.assertIn("unchanged cron", report)
        self.assertIn("none: every family advertises the same actions", report)

    def test_the_tool_rejects_an_incomplete_invocation(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT)], capture_output=True, text=True, check=False
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--old-tag", result.stderr)


def capture_report(old, new, old_contract=None, new_contract=None) -> str:
    """Run the report and capture what it prints."""
    import io
    import contextlib

    buffer = io.StringIO()
    with contextlib.redirect_stdout(buffer):
        compare.report(old, new, old_contract, new_contract)
    return buffer.getvalue()


if __name__ == "__main__":
    unittest.main()
