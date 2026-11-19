#!/usr/bin/env bash
# Tests install.sh's release-tag resolution without reaching the network.
#
# The installer must find the newest published Enact Agent distribution even
# when that publication is a pre-release. GitHub's Releases "latest" endpoint
# reports only non-prerelease, non-draft releases, so it answers 404 for this
# fork's entire release history. Resolution therefore reads the releases list
# instead, and this test pins that behaviour against a fixture API payload.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# The tracked installer is upstream's, so the release resolution the fork
# ships lives in the overlay file store. The test exercises the fork's
# installer, not the tracked one.
INSTALL_SH="$ROOT/overlays/enact/files/install.sh"

if [[ ! -f "$INSTALL_SH" ]]; then
  echo "FAIL: install.sh not found at $INSTALL_SH"
  exit 1
fi

tmp="$(mktemp)"
url_file="$(mktemp)"
trap 'rm -f "$tmp" "$url_file"' EXIT

if ! awk '
  /^resolve_latest_release_tag\(\) \{/ { in_func = 1 }
  in_func { print }
  in_func && /^}$/ { found = 1; exit }
  END { if (!found) exit 1 }
' "$INSTALL_SH" >"$tmp"; then
  echo "FAIL: could not extract resolve_latest_release_tag() from install.sh"
  exit 1
fi

bash -n "$tmp"

# shellcheck disable=SC1090
source "$tmp"

pass=0
fail=0
failures=()

# The fixture replaces the network call with a canned payload. The URL is
# still asserted, because the releases list and the "latest" endpoint are
# different resources and only the list reports pre-releases.
fixture_payload=""
fixture_url_file="$url_file"

curl() {
  # The production call carries flags (`-fsSL`), a redirect (`2>/dev/null`)
  # and the URL. Record the last argument that looks like a URL so the test
  # can assert which endpoint was consulted. The resolution runs in a command
  # substitution — a subshell — so the URL is written to a file the parent can
  # read rather than assigned to a shell variable.
  local arg
  for arg in "$@"; do
    case "$arg" in
      https://*) printf '%s' "$arg" >"$fixture_url_file" ;;
    esac
  done
  printf '%s' "$fixture_payload"
}

assert_resolution() {
  local label="$1" payload="$2" expected="$3" actual
  fixture_payload="$payload"
  actual="$(resolve_latest_release_tag)"
  if [[ "$actual" == "$expected" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("${label}: expected '${expected}', got '${actual}'")
  fi
}

# The fork's actual shape: every publication is a pre-release, and the
# releases list is the only endpoint that reports them.
assert_resolution \
  'newest pre-release is found' \
  '[
  {
    "tag_name": "enact-agent-v0.9.0-enact.1",
    "prerelease": true,
    "draft": false
  },
  {
    "tag_name": "enact-agent-v0.8.7-enact.1",
    "prerelease": true,
    "draft": false
  }
]' \
  'enact-agent-v0.9.0-enact.1'

# A non-prerelease publication is equally acceptable.
assert_resolution \
  'newest stable release is found' \
  '[{"tag_name": "enact-agent-v0.9.0-enact.2", "prerelease": false, "draft": false}]' \
  'enact-agent-v0.9.0-enact.2'

# A payload with no distribution tag at all resolves to nothing, which the
# caller turns into a source-build fallback rather than a bad download.
assert_resolution \
  'no distribution tag yields nothing' \
  '[{"tag_name": "v0.9.0", "prerelease": false, "draft": false}]' \
  ''

# An empty payload behaves the same way.
assert_resolution \
  'empty payload yields nothing' \
  '' \
  ''

# The resolution must read the releases list, not the "latest" endpoint: the
# latter omits pre-releases, which is the exact case this fix exists for.
fixture_payload='[{"tag_name": "enact-agent-v0.9.0-enact.1"}]'
resolve_latest_release_tag >/dev/null
fixture_url="$(cat "$url_file")"
if [[ "$fixture_url" == *"/releases?per_page=100"* ]]; then
  pass=$((pass + 1))
else
  fail=$((fail + 1))
  failures+=("resolution URL: expected the releases list, got '${fixture_url}'")
fi

# The releases list reports drafts alongside published releases. A draft is
# not yet a distribution, so it must be skipped in favour of the newest
# published tag.
assert_resolution \
  'a draft is skipped' \
  '[{"tag_name": "enact-agent-v0.9.0-enact.3", "draft": true},
   {"tag_name": "enact-agent-v0.9.0-enact.2", "draft": false}]' \
  'enact-agent-v0.9.0-enact.2'

# A payload that contains only drafts resolves to nothing, which the caller
# turns into a source-build fallback rather than a bad download.
assert_resolution \
  'only drafts yields nothing' \
  '[{"tag_name": "enact-agent-v0.9.0-enact.3", "draft": true}]' \
  ''

echo "passed: $pass"
echo "failed: $fail"

if (( fail > 0 )); then
  echo
  echo "Failures:"
  for failure in "${failures[@]}"; do
    echo "  - $failure"
  done
  exit 1
fi
