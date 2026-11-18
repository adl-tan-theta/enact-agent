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
    def test_a_release_without_a_family_contract_reports_none(self):
        self.assertEqual(compare.families_of(manifest()), {})
        self.assertEqual(compare.families_of(manifest(families=None)), {})
        self.assertEqual(compare.families_of(manifest(families={})), {})

    def test_families_are_read_from_the_manifest(self):
        families = {"cron": {"cron_add": {}, "cron_run": {}}}
        self.assertEqual(compare.families_of(manifest(families=families)), families)

    def test_a_release_with_no_contract_shows_the_whole_contract_arriving(self):
        old = manifest()
        new = manifest(
            families={
                "cron": {"cron_add": {}, "cron_run": {}},
                "sop": {"sop_list": {}},
            }
        )
        report = capture_report(old, new)
        self.assertIn("added     2 families", report)
        self.assertIn("cron", report)
        self.assertIn("sop", report)

    def test_action_changes_within_a_family_are_reported(self):
        old = manifest(families={"cron": {"cron_add": {}, "cron_run": {}}})
        new = manifest(families={"cron": {"cron_add": {}, "cron_pause": {}}})
        report = capture_report(old, new)
        self.assertIn("Action changes", report)
        self.assertIn("+ cron_pause", report)
        self.assertIn("- cron_run", report)

    def test_unchanged_families_are_reported_as_unchanged(self):
        families = {"cron": {"cron_add": {}}}
        report = capture_report(manifest(families=families), manifest(families=families))
        self.assertIn("unchanged cron", report)
        self.assertIn("none: every family advertises the same actions", report)

    def test_the_tool_rejects_an_incomplete_invocation(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT)], capture_output=True, text=True, check=False
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--old-tag", result.stderr)


def capture_report(old, new) -> str:
    """Run the report and capture what it prints."""
    import io
    import contextlib

    buffer = io.StringIO()
    with contextlib.redirect_stdout(buffer):
        compare.report(old, new)
    return buffer.getvalue()


if __name__ == "__main__":
    unittest.main()
