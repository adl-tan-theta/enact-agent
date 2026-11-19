#!/usr/bin/env bash
# The fork's docs link gate.
#
# It is upstream's gate with one change: the collector is the fork's, because
# the fork keeps a whole-file copy of an upstream page in the overlay file
# store. That copy's relative links resolve against the tracked tree the page
# is projected into, not against the store, so the collector skips the store
# rather than reporting every one of those links as broken.
#
# The tracked `scripts/ci/docs_links_gate.sh` stays upstream's, and so do the
# tracked collector and its test. The fork's own copies live beside it.

set -euo pipefail

BASE_SHA="${BASE_SHA:-}"
DOCS_FILES_RAW="${DOCS_FILES:-}"

LINKS_FILE="$(mktemp)"
HTTP_LINKS_FILE="$(mktemp)"
trap 'rm -f "$LINKS_FILE" "$HTTP_LINKS_FILE"' EXIT

python3 ./scripts/ci/enact/collect_changed_links_test.py

python3 ./scripts/ci/enact/collect_changed_links.py \
    --base "$BASE_SHA" \
    --docs-files "$DOCS_FILES_RAW" \
    --output "$LINKS_FILE" \
    --http-output "$HTTP_LINKS_FILE" \
    --check-local-targets

if [ ! -s "$LINKS_FILE" ]; then
    echo "No added links detected in changed docs lines."
    exit 0
fi

if [ ! -s "$HTTP_LINKS_FILE" ]; then
    echo "No added HTTP(S) links detected in changed docs lines."
    exit 0
fi

if ! command -v lychee >/dev/null 2>&1; then
    echo "Added HTTP(S) links detected, but lychee is not installed; skipping optional HTTP(S) validation."
    echo "Install via: `cargo install lychee`"
    exit 0
fi

echo "Checking added HTTP(S) links with lychee (offline mode)..."
lychee --offline --no-progress --format detailed "$HTTP_LINKS_FILE"
