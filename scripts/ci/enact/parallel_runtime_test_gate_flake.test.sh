#!/usr/bin/env bash
# Tests the parallel runtime gate's flake absorption.
#
# The gate runs the suite repeatedly on purpose, because a timing-sensitive
# test can fail on a loaded machine and pass on the next run. `set -e` used to
# abort on the first failure, so the repetition never got the chance it was
# written for. The gate now absorbs a failure when a later run of the same
# crate passes, and still fails when every run of that crate failed.
#
# The suite is stubbed: a fake `cargo` on PATH decides each run's outcome, so
# this test exercises the gate's control flow without compiling the workspace.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
gate="${script_dir}/parallel_runtime_test_gate.sh"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# A fixture whose fake `cargo` fails the first `fail_times` runs of each crate
# and then succeeds. `ZEROCLAW_FIXTURE_LOG` records the outcome per run.
make_fixture() {
    fixture="$(mktemp -d)"
    mkdir -p "$fixture/bin"
    cat >"$fixture/bin/cargo" <<'EOF'
#!/bin/sh
count_file="$ZEROCLAW_FIXTURE_STATE/count"
count=0
if [ -f "$count_file" ]; then
    count="$(cat "$count_file")"
fi
count=$((count + 1))
printf '%s' "$count" >"$count_file"
echo "run $count" >>"$ZEROCLAW_FIXTURE_LOG"
if [ "$count" -le "$ZEROCLAW_FIXTURE_FAIL_TIMES" ]; then
    echo "simulated failure on run $count" >&2
    exit 101
fi
exit 0
EOF
    chmod +x "$fixture/bin/cargo"
}

run_gate() {
    local scope="$1"
    local runs="$2"
    local fail_times="$3"
    set +e
    output="$(
        ZEROCLAW_PARALLEL_TEST_SCOPE="$scope" \
        ZEROCLAW_PARALLEL_TEST_RUNS="$runs" \
        ZEROCLAW_PARALLEL_TEST_THREADS=1 \
        ZEROCLAW_FIXTURE_STATE="$fixture" \
        ZEROCLAW_FIXTURE_LOG="$fixture/log" \
        ZEROCLAW_FIXTURE_FAIL_TIMES="$fail_times" \
        PATH="$fixture/bin:$PATH" \
        bash "$gate" 2>&1
    )"
    status=$?
    set -e
}

# ── Case 1: a flake in the first run is absorbed by a later pass ────────────
make_fixture >/dev/null
run_gate all 3 1
if [[ "$status" -ne 0 ]]; then
    echo "$output" >&2
    fail "a run that failed once and then passed should not fail the gate"
fi
if ! grep -q "at least one run passed" <<<"$output"; then
    echo "$output" >&2
    fail "the gate should report that a later run passed"
fi
if grep -q "failed in all" <<<"$output"; then
    echo "$output" >&2
    fail "the gate should not report a total failure"
fi
# The gate runs both crates in the `all` scope, so there are 3 runs per crate.
runs_logged="$(wc -l <"$fixture/log" | tr -d ' ')"
if [[ "$runs_logged" -lt 4 ]]; then
    fail "the gate should have retried after the failed run (logged $runs_logged runs)"
fi

# ── Case 2: every run failing still fails the gate ─────────────────────────
make_fixture >/dev/null
run_gate all 3 99
if [[ "$status" -eq 0 ]]; then
    echo "$output" >&2
    fail "a crate that fails in every run must fail the gate"
fi
if ! grep -q "failed in all 3 runs" <<<"$output"; then
    echo "$output" >&2
    fail "the gate should report that every run failed"
fi

# ── Case 3: the channels scope still gates its own crate ───────────────────
make_fixture >/dev/null
run_gate channels 2 99
if [[ "$status" -eq 0 ]]; then
    echo "$output" >&2
    fail "a crate that fails in every run must fail the gate"
fi

echo "parallel runtime test gate flake tests: pass"
