#!/usr/bin/env bash
# Run the workspace tests against the prepared tree.
#
# The tracked tree is upstream's bytes, so it carries upstream's behaviour.
# The prepared tree is what the release ships, and it is where the fork's
# overlays apply -- including the Bedrock provider's metadata-service opt-out,
# which the fork's own CI depends on.
#
# `cargo nextest` needs the prepared tree's own `Cargo.lock` and the pinned
# toolchain. The prepared tree is a fresh copy of this repository's commit
# with the overlays applied, so the lockfile is already the one the workspace
# resolves against.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"
CARGO_BIN="${CARGO_BIN:-cargo}"

cd "$ROOT"

# The preparation is the fork's own overlay applied to this commit. It writes
# into `target/enact/source/`, which the workspace's `target/` ignores.
"$PYTHON_BIN" -B overlays/enact/build.py prepare

source_dir="$ROOT/target/enact/source"

# The prepared tree carries the fork's `Cargo.lock` and the workspace
# manifest, so the tests resolve the same dependency graph the release does.
cd "$source_dir"

exec "$CARGO_BIN" nextest run --locked --workspace --exclude zeroclaw-desktop "$@"
