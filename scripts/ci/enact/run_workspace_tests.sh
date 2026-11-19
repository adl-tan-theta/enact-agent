#!/usr/bin/env bash
# Run the workspace tests, with the provider crate's tests from the prepared
# tree.
#
# The tracked tree is upstream's bytes, so it carries upstream's behaviour.
# The prepared tree is what the release ships, and it is where the fork's
# overlays apply -- including the Bedrock provider's metadata-service
# opt-out.
#
# That opt-out matters for exactly one test.
# `bedrock::tests::chat_fails_without_credentials` asserts that a request
# with no credentials fails on the missing credential and nothing else. The
# provider reads the instance metadata service as its last credential source,
# and that address is not exclusively AWS's: a GitHub-hosted runner is an
# Azure VM where it answers with something that is not an AWS metadata
# document. The provider then fails on the shape of the response rather than
# on the missing credential. Upstream never sees this because its fleet runs
# on Blacksmith, where the address is not reachable.
#
# Running the whole workspace from the prepared tree would compile it a second
# time from cold, which does not fit in the job's ceiling. The provider
# crate's own dependencies are already built for the tracked tree, so
# building it in the prepared tree costs the provider crate and little else.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"
CARGO_BIN="${CARGO_BIN:-cargo}"

cd "$ROOT"

# The preparation is the fork's own overlay applied to this commit. It writes
# into `target/enact/source/`, which the workspace's `target/` ignores.
"$PYTHON_BIN" -B overlays/enact/build.py prepare

source_dir="$ROOT/target/enact/source"

# The tracked tree's tests, with the workspace's own target directory, so the
# compiled artifacts and the dependency cache are the ones the rest of the
# job already built.
CARGO_TARGET_DIR="$ROOT/target" \
    "$CARGO_BIN" nextest run --locked --workspace --exclude zeroclaw-desktop \
    --exclude zeroclaw-providers "$@"

# The provider crate's tests, from the prepared tree, where the fork's
# metadata-service opt-out applies.
(
    cd "$source_dir"
    export CARGO_TARGET_DIR="$ROOT/target"
    "$CARGO_BIN" nextest run --locked -p zeroclaw-providers "$@"
)
