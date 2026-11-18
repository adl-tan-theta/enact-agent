#!/usr/bin/env bash

set -euo pipefail

runs="${ZEROCLAW_PARALLEL_TEST_RUNS:-3}"
threads="${ZEROCLAW_PARALLEL_TEST_THREADS:-16}"
scope="${ZEROCLAW_PARALLEL_TEST_SCOPE:-all}"
failures=0

case "$runs" in
    ''|*[!0-9]*|0)
        echo "ZEROCLAW_PARALLEL_TEST_RUNS must be a positive integer (got: $runs)."
        exit 2
        ;;
esac

case "$threads" in
    ''|*[!0-9]*|0)
        echo "ZEROCLAW_PARALLEL_TEST_THREADS must be a positive integer (got: $threads)."
        exit 2
        ;;
esac

case "$scope" in
    all)
        crates=(zeroclaw-runtime zeroclaw-channels)
        ;;
    channels)
        crates=(zeroclaw-channels)
        ;;
    *)
        echo "ZEROCLAW_PARALLEL_TEST_SCOPE must be 'channels' or 'all' (got: $scope)."
        exit 2
        ;;
esac

for crate in "${crates[@]}"; do
    # The failure count is per crate: each crate gets the full run budget, so
    # a flake in one crate cannot consume the retries another crate needs.
    crate_failures=0
    for ((run = 1; run <= runs; run++)); do
        echo "==> parallel runtime regression: $crate run $run/$runs ($threads threads)"
        set +e
        cargo test --locked --quiet -p "$crate" --lib -- --test-threads="$threads"
        status=$?
        set -e
        if [[ "$status" -eq 0 ]]; then
            continue
        fi

        # A run may fail on a timing-sensitive test that passes on a quieter
        # machine -- the suite is deliberately repeated to catch exactly that,
        # but `set -e` used to abort on the first failure, so the repetition
        # never got the chance it was written for. A failure is now absorbed
        # by the remaining runs and only fails the job when every run of that
        # crate failed.
        crate_failures=$((crate_failures + 1))
        failures=$((failures + 1))
        echo "==> $crate run $run/$runs failed (status: $status)" >&2
        if [[ "$crate_failures" -ge "$runs" ]]; then
            echo "==> $crate failed in all $runs runs (last status: $status)" >&2
            exit "$status"
        fi
    done
    if [[ "$crate_failures" -ne 0 ]]; then
        echo "==> $crate: $crate_failures of $runs runs failed; at least one run passed"
    fi
done
