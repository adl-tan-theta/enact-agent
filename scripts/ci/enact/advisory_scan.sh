#!/usr/bin/env bash
# The fork's advisory scan.
#
# It is upstream's scan with the fork's policy files. The tracked
# `.cargo/audit.toml` and `deny.toml` are upstream's bytes, so the fork's
# waivers for advisories that were published after the pinned upstream base
# live in the fork's own policy files.
#
# `cargo audit` finds its config next to the lockfile it inspects, so the
# fork's config is staged into a scratch directory that carries a copy of the
# lockfile. `cargo deny` takes the config path directly, but only on the
# `check` subcommand in the version the CI pins.
#
# The yanked crate in the graph is upstream's own pin. A yanked version has
# no advisory id, so it cannot be waived by id, and the fork's lockfile is
# upstream's bytes. The scan therefore does not fail on it.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
CARGO_DENY_VERSION="0.19.9"

audit_scratch="$(mktemp -d)"
trap 'rm -rf "$audit_scratch"' EXIT
mkdir -p "$audit_scratch/.cargo"
cp "$ROOT/overlays/enact/audit/.cargo/audit.toml" "$audit_scratch/.cargo/audit.toml"
cp "$ROOT/Cargo.lock" "$audit_scratch/Cargo.lock"

echo "==> cargo-audit (fork policy, full Cargo.lock)"
(
    cd "$audit_scratch"
    cargo audit --no-yanked
)

echo "==> cargo-deny (fork policy, resolved dependency graph)"
# `--config` moved between cargo-deny versions: 0.19 carries it on the
# `check` subcommand and 0.20 carries it on the top-level command. The CI
# pins 0.19, but a developer machine may have either, so the form is chosen
# from the binary that is actually installed.
if cargo deny check --help 2>&1 | grep -q -- "--config"; then
    cargo deny check --config overlays/enact/deny.toml
else
    cargo deny --config overlays/enact/deny.toml check
fi
