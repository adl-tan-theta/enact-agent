#!/usr/bin/env bash
# Tests the PowerShell interpreter guard.
#
# The guard removes a broken PowerShell from PATH so the Windows-only
# PowerShell tests skip themselves, exactly as they do on upstream's fleet
# where no PowerShell is installed. A working PowerShell is left alone.

set -euo pipefail

bash="$(command -v bash)"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
guard="${script_dir}/powershell_interpreter_guard.sh"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

make_fixture() {
    # $1: "broken" or "working"
    fixture="$(mktemp -d)"
    mkdir -p "$fixture/bin"
    if [[ "$1" == "working" ]]; then
        cat >"$fixture/bin/pwsh" <<'EOF'
#!/bin/sh
# A working interpreter: echoes the probe script's output.
for arg in "$@"; do
    case "$arg" in
        Write-Output*) echo "ci-powershell-probe" ;;
    esac
done
exit 0
EOF
    else
        cat >"$fixture/bin/pwsh" <<'EOF'
#!/bin/sh
# A broken interpreter: `exit 0` works, a real script dies.
for arg in "$@"; do
    case "$arg" in
        exit) exit 0 ;;
    esac
done
echo "System.IO.FileLoadException: The given assembly name was invalid." >&2
exit 1
EOF
    fi
    chmod +x "$fixture/bin/pwsh"
}

# ── Case 1: no PowerShell at all ───────────────────────────────────────────
# The PATH holds only the fixture's empty bin, so no interpreter is found.
fixture="$(mktemp -d)"
mkdir -p "$fixture/bin"
for tool in bash cat command dirname mkdir mktemp rm sh; do
    target="$(command -v "$tool" 2>/dev/null || true)"
    [ -n "$target" ] && ln -sf "$target" "$fixture/bin/$tool"
done
output="$(PATH="$fixture/bin" "$bash" "$guard" 2>&1)"
if [[ "$output" != *"No PowerShell interpreter"* ]]; then
    echo "$output" >&2
    fail "the guard should report that there is no interpreter"
fi

# ── Case 2: a working PowerShell is left alone ─────────────────────────────
make_fixture working
output="$(PATH="$fixture/bin" ZEROCLAW_CI_POWERSHELL_SHADOW_DIR="$fixture/shadow" "$bash" "$guard" 2>&1)"
if [[ "$output" != *"leaving it on PATH"* ]]; then
    echo "$output" >&2
    fail "the guard should leave a working PowerShell alone"
fi
if [[ -d "$fixture/shadow" ]]; then
    fail "the guard must not create a shadow directory for a working PowerShell"
fi

# ── Case 3: a broken PowerShell is shadowed ────────────────────────────────
make_fixture broken
output="$(PATH="$fixture/bin" ZEROCLAW_CI_POWERSHELL_SHADOW_DIR="$fixture/shadow" "$bash" "$guard" 2>&1)"
if [[ "$output" != *"cannot execute a script"* ]]; then
    echo "$output" >&2
    fail "the guard should report that the interpreter cannot run a script"
fi
if [[ ! -x "$fixture/shadow/pwsh" ]]; then
    fail "the guard should shadow the broken interpreter"
fi
# The shadow must refuse to run, so the tests' probe finds a name that fails.
set +e
"$fixture/shadow/pwsh" -NoProfile -Command "exit 0" >/dev/null 2>&1
shadow_status=$?
set -e
if [[ "$shadow_status" -eq 0 ]]; then
    fail "the shadow interpreter must not exit successfully"
fi

# ── Case 4: the guard publishes the shadow directory through GITHUB_PATH ────
make_fixture broken
github_path="$fixture/github_path"
: >"$github_path"
output="$(PATH="$fixture/bin" GITHUB_PATH="$github_path"     ZEROCLAW_CI_POWERSHELL_SHADOW_DIR="$fixture/shadow" "$bash" "$guard" 2>&1)"
if [[ "$output" != *"$fixture/shadow"* ]]; then
    echo "$output" >&2
    fail "the guard should report the shadow directory"
fi
if ! grep -q "$fixture/shadow" "$github_path"; then
    fail "the guard must publish the shadow directory through GITHUB_PATH"
fi

echo "powershell interpreter guard tests: pass"
