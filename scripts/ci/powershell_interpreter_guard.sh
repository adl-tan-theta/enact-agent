#!/usr/bin/env bash
# Make the runner's PowerShell state match what upstream's fleet provides.
#
# `zeroclaw-config`'s PowerShell tests are Windows-only in intent: they look
# for a `pwsh`/`powershell` interpreter and skip themselves when there is
# none. Upstream's Blacksmith runners have no PowerShell, so those tests always
# skip there.
#
# The GitHub-hosted Ubuntu image does ship PowerShell, and on 2026-11-18 the
# copy it ships was broken: `pwsh -NoProfile -Command "exit 0"` starts and
# exits cleanly -- which is all the test's probe checks -- but a real script
# dies with `System.IO.FileLoadException: The given assembly name was
# invalid`. Three PowerShell tests then failed on a fork that ships no
# Windows build, for a reason that had nothing to do with the code.
#
# The probe is too weak to notice that. So the interpreter is removed from
# PATH for the test run when it cannot execute a real script, and the tests
# skip themselves exactly as they do upstream. When PowerShell works, nothing
# is removed and the tests run for real.

set -euo pipefail

# The guard is expected to run with a PATH that holds only the shadow
# directory, so the handful of tools it needs are resolved once, up front,
# against the PATH the job starts with. The standard locations are searched as
# a fallback because the guard may itself be invoked with a stripped PATH.
resolve_tool() {
    local tool="$1"
    local candidate
    if candidate="$(command -v "$tool" 2>/dev/null)" && [[ -n "$candidate" ]]; then
        printf '%s' "$candidate"
        return 0
    fi
    for candidate in "/usr/bin/$tool" "/bin/$tool"; do
        if [[ -x "$candidate" ]]; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    return 1
}

mkdir_bin="$(resolve_tool mkdir)"
cat_bin="$(resolve_tool cat)"
chmod_bin="$(resolve_tool chmod)"

probe_script='Write-Output ci-powershell-probe'

interpreter=""
for candidate in pwsh powershell; do
    if command -v "$candidate" >/dev/null 2>&1; then
        interpreter="$candidate"
        break
    fi
done

if [[ -z "$interpreter" ]]; then
    echo "No PowerShell interpreter on PATH; the PowerShell tests will skip themselves."
    exit 0
fi

if output="$("$interpreter" -NoProfile -NonInteractive -Command "$probe_script" 2>&1)" \
    && [[ "$output" == "ci-powershell-probe" ]]; then
    echo "PowerShell ($interpreter) executes scripts; leaving it on PATH."
    exit 0
fi

echo "PowerShell ($interpreter) cannot execute a script; removing it from PATH." >&2
echo "  output: ${output:-<none>}" >&2

# Shadow the interpreter with a directory that holds a stub that always fails,
# so `command -v pwsh` still finds a name but the spawn fails and the tests
# skip themselves. Removing the real binary from the image is not this job's
# job.
shadow_dir="${ZEROCLAW_CI_POWERSHELL_SHADOW_DIR:-${RUNNER_TEMP:-/tmp}/ci-no-powershell}"
"$mkdir_bin" -p "$shadow_dir"
for candidate in pwsh powershell; do
    "$cat_bin" >"$shadow_dir/$candidate" <<EOF
#!/bin/sh
echo "PowerShell is unavailable on this runner." >&2
exit 127
EOF
    "$chmod_bin" +x "$shadow_dir/$candidate"
done

# `$GITHUB_PATH` is the supported way to change the PATH of the *following*
# steps. Prepending the shadow directory puts the failing stub ahead of the
# real interpreter.
if [[ -n "${GITHUB_PATH:-}" ]]; then
    printf '%s\n' "$shadow_dir" >> "$GITHUB_PATH"
fi

echo "$shadow_dir"
