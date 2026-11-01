# Enact Agent overlay contract

Read the [root instructions](../../AGENTS.md) and
[architecture map](../../docs/book/src/contributing/architecture-map.md) first.
This contract applies to Enact-specific fork maintenance, native changes,
web branding, installation, releases and upstream synchronization.
It supplements upstream rules; it does not relax them.

## Sources of truth

Read the relevant owner before changing behavior or documenting a claim.

| Owner | Canonical facts |
| --- | --- |
| [brand.json](brand.json) | Display name, public command, accent, light/dark logo filenames |
| [release.json](release.json) | Distribution version, upstream/fork repository URLs, pinned upstream source base |
| [build.py](build.py) | Prepared-source transformations, native feature selection, provenance, archive and privacy guards |
| [Cargo.toml](../../Cargo.toml) and [Cargo.lock](../../Cargo.lock) | Upstream runtime version, declared Rust floor, feature definitions, locked dependencies |
| [probe_native.py](probe_native.py) and [test_build.py](test_build.py) | Native boundary qualification and focused overlay checks |
| [native workflow](../../.github/workflows/enact-agent-build.yml) | Manual native runner matrix, selected toolchain, extracted-archive fixture |
| [web branding helper](../../web/src/lib/branding.ts) and `web/public/enact-logo*.svg` | Dashboard presentation and canonical vector assets |

Use [README.md](README.md) for optional dashboard/desktop build prerequisites.
Use the [public release](https://github.com/adl-tan-theta/enact-agent/releases/tag/enact-agent-v0.8.7-enact.1)
for published asset availability and checksums. Do not treat a local build or a
successful compile as proof that a release asset exists or passes its fixture.

## Boundaries and required behavior

- Preserve upstream `origin`, the `enact` fork remote, licenses, copyrights and
  `NOTICE`. Work on review branches; never edit or push directly to `master`.
- Keep fork-only native branding/RPC adaptations in `build.py`, not tracked
  `src/` or `crates/` Rust. Upstream merges may update upstream Rust normally.
- `prepare` deletes/recreates `target/enact/source/`. Never hand-edit this output
  as the durable fix, commit binaries, or prepare/build concurrently in one worktree.
- Tracked `web/` changes cover Enact presentation, canonical assets and brand
  wiring. Preserve API/config identifiers, i18n keys and security semantics;
  review new behavior separately. Default system-prompt/personality identity
  branding is model-visible, not merely a logo change.
- No blanket replacements, crate/module renames or renamed compatibility paths.
  Native environment projection changes Rust string literals in prepared sources.
- Preserve `ENACT_AGENT_*`, `enact-agent-local-rpc` v1 over Unix NDJSON and the
  exact manifest-v1 shape. Review consumer compatibility before contract changes.
- Preserve actual constructed `session/tools` inventory, owner/incarnation
  checks, approvals, permissions and sandboxing. Configured MCP entries do not
  prove connection. Never bypass security to make qualification pass.
- Forward real local tool status; provider-preexecuted status is `success: null`.
  Never infer a boolean from output. Consumers requiring definite status must
  reject unknown outcomes.
- Do not read secret stores, credential files or `.env` files, or expose keys in
  commands/logs/archives. The native fixture uses isolated temporary configuration,
  local endpoints and no paid calls.
- Stop on drift or failed checks; never silently skip transformations, weaken
  guards, or substitute an older binary. Keep source, privacy and attribution checks.
- Never overwrite release bytes or move published tags. `0.8.6-enact.1` remains
  withdrawn; do not install or republish it. Changed artifacts require a new
  version/tag.

Upstream may change. There is no guarantee that future merges cannot break the
fork. Checked single-match anchors and constructor counts fail closed on known
drift; they do not prove semantic compatibility. An anchor failure blocks release
until reviewed adaptation and target requalification pass.

## Install the published headless release

Release `0.8.7-enact.1` has qualified native archives for:

- Apple Silicon macOS: `aarch64-apple-darwin`.
- Intel macOS: `x86_64-apple-darwin`.
- Linux x64: `x86_64-unknown-linux-gnu`.

All three use fork source `c99096a8b9e8c50726b72a032553f7727babfa6c` on upstream
base `1c1cd118ac536a1dd1bc79b9cce69cbdc04e4f29` and passed native
extracted-archive fixtures. Other targets are unqualified. Later docs commits do
not change these binaries' provenance.

Prebuilt installation needs `curl`, `tar` and `shasum` (macOS) or `sha256sum`
(Linux). It needs no npm, Cargo, Rust, Nix or provider credentials. Use a new
download directory; stop on any download, extraction or checksum failure.

```sh
set -eu
case "$(uname -s):$(uname -m)" in
  Darwin:arm64) target=aarch64-apple-darwin ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
  *) printf '%s\n' 'No qualified archive for this operating system/CPU.' >&2; exit 1 ;;
esac
version=0.8.7-enact.1
tag=enact-agent-v0.8.7-enact.1
archive="$tag-$target.tar.gz"
mkdir "enact-agent-$version-download"
cd "enact-agent-$version-download"
base="https://github.com/adl-tan-theta/enact-agent/releases/download/$tag"
curl --fail --location --output "$archive" "$base/$archive"
curl --fail --location --output SHA256SUMS "$base/SHA256SUMS-0.8.7-enact.1"
awk -v file="$archive" '$2 == file { print; count++ } END { if (count != 1) exit 1 }' SHA256SUMS > SELECTED-SHA256SUMS
case "$(uname -s)" in
  Darwin) shasum -a 256 -c SELECTED-SHA256SUMS ;;
  Linux) sha256sum -c SELECTED-SHA256SUMS ;;
esac
tar -xzf "$archive"
cd "$tag-$target"
case "$(uname -s)" in
  Darwin) shasum -a 256 -c SHA256SUMS ;;
  Linux) sha256sum -c SHA256SUMS ;;
esac
./enact-agent --version
mkdir -p "$HOME/.local/share/enact-agent" "$HOME/.local/bin"
test ! -e "$HOME/.local/share/enact-agent/$version"
cp -R . "$HOME/.local/share/enact-agent/$version"
ln -sfn "$HOME/.local/share/enact-agent/$version/enact-agent" "$HOME/.local/bin/enact-agent"
"$HOME/.local/bin/enact-agent" --help
```

The versioned outer asset `SHA256SUMS-0.8.7-enact.1` verifies archives; the inner
`SHA256SUMS` verifies payload files. Preserve all 11 files: executable,
`manifest.json`, `RELEASE.json`, both licenses, `NOTICE`, `UPSTREAM.txt`,
`INSTALL.txt`, both `branding/` SVGs and inner checksums. Do not copy only the
binary. The launcher replacement needs no root access; retain an older verified
installation for rollback, except withdrawn `0.8.6-enact.1`.

These headless archives contain no dashboard/desktop app, notarization or GitHub
build attestation. Checksums establish integrity, not publisher identity. Do not
silently disable OS security. Add `$HOME/.local/bin` to PATH or use the explicit
launcher. Configure a provider separately with `enact-agent quickstart`, then
`enact-agent daemon` when appropriate. Installation does not migrate workspaces;
do not read or overwrite private configuration/data during verification.

## Clone source without replacing upstream origin

Prebuilt users need no checkout. To pin published source on a named branch
while retaining both repositories:

```sh
git clone --origin enact --branch enact/branding https://github.com/adl-tan-theta/enact-agent.git enact-agent-source
cd enact-agent-source
git remote add origin https://github.com/zeroclaw-labs/zeroclaw.git
git switch --create enact/release-0.8.7-enact.1 c99096a8b9e8c50726b72a032553f7727babfa6c
git remote get-url origin
git remote get-url enact
```

Verify both remote URLs. The branded updater refuses upstream binary downloads,
which would remove the overlay. Do not substitute the upstream installer.

## Review an upstream update in an isolated worktree

From a clean fork checkout, create a new sibling review worktree. This leaves
`master` and existing work intact and stops before committing the merge.
These are maintenance commands, not installation steps.

```sh
set -eu
test -z "$(git status --porcelain=v1 --untracked-files=all)"
test "$(git remote get-url origin)" = https://github.com/zeroclaw-labs/zeroclaw.git
test "$(git remote get-url enact)" = https://github.com/adl-tan-theta/enact-agent.git
git fetch origin
git fetch enact
review_branch="enact/upstream-$(date -u +%Y%m%dT%H%M%SZ)"
git worktree add -b "$review_branch" ../enact-upstream-review enact/enact/branding
cd ../enact-upstream-review
git merge --no-commit --no-ff origin/master
```

Choose an unused destination if that worktree exists. Resolve conflicts with
review; do not select ours/theirs across source trees. Inspect affected RPC,
env parsing, i18n, features, tool factories, security, web and release owners.

Select `release.json`'s upstream base deliberately after review; inspect
`git rev-parse origin/master`, not fork merge `HEAD`. The base must be an ancestor
of the final fork commit. Reconcile distribution version with `Cargo.toml`;
changed published artifacts need a new version. Preserve upstream root rules
and this fork's pointer. Review/validate before committing; release generation
requires a clean committed tree.

After choosing that base, require the tracked core to match it:

```sh
upstream_base="$(python3 -c 'import json; print(json.load(open("overlays/enact/release.json"))["upstreamCommit"])')"
git diff --exit-code "$upstream_base" -- src crates Cargo.toml Cargo.lock
```

This check is clean for the published `.1` source. Stop review on unexpected
fork-only core differences; investigate rather than rewriting files automatically.

## Validation and release gate

For overlay/upstream changes, run focused checks and inspect generated changes.
Also run relevant upstream CI from the architecture map; this fixture does not
replace required fmt, clippy, tests or CI gates for affected upstream surfaces.

```sh
python3 -B -m unittest discover -s overlays/enact -p test_build.py -v
git diff --check
```

When web presentation or its upstream dependencies changed, run from `web/`:

```sh
bun --no-env-file test ./src/lib/branding.test.ts ./src/components/layout/Sidebar.test.ts
./node_modules/.bin/tsc -b --pretty false
```

Check actual light/dark UI output for presentation changes and relevant upstream
web checks for broader changes. Docs-only changes use existing quality/link gates
with explicit `DOCS_FILES` for these outside-book files; default discovery can
skip them. Do not report a skipped gate as file validation.

Build a clean approved commit with Python 3.11+, the declared Rust toolchain
already installed in rustup, and cached locked dependencies. Resolve tool paths
for that exact toolchain and clear inherited env.
Prefetch locked dependencies separately as the native workflow does if needed;
do not remove offline mode to hide a failure.

```sh
set -eu
test -z "$(git status --porcelain=v1 --untracked-files=all)"
python_bin="$(command -v python3)"
rust_version="$("$python_bin" -c 'import pathlib,tomllib; print(tomllib.loads(pathlib.Path("Cargo.toml").read_text())["workspace"]["package"]["rust-version"])')"
cargo_bin="$(rustup which --toolchain "$rust_version" cargo)"
rustc_bin="$(rustup which --toolchain "$rust_version" rustc)"
env -i HOME="$HOME" PATH="$PATH" LANG=C.UTF-8 "$python_bin" -B overlays/enact/build.py release --cargo "$cargo_bin" --rustc "$rustc_bin"
target="$("$rustc_bin" -vV | sed -n 's/^host: //p')"
tag="$("$python_bin" -c 'import json; r=json.load(open("overlays/enact/release.json")); b=json.load(open("overlays/enact/brand.json")); print(b["command"]+"-v"+r["version"])')"
release_dir="target/enact/releases/$tag"
stem="$tag-$target"
mkdir "$release_dir/reviewer-extracted"
tar -xzf "$release_dir/$stem.tar.gz" -C "$release_dir/reviewer-extracted"
env -i HOME="$HOME" PATH="$PATH" LANG=C.UTF-8 "$python_bin" -B overlays/enact/probe_native.py --binary "$release_dir/reviewer-extracted/$stem/enact-agent"
```

Release output must be new; `--output-dir` accepts a new absolute directory
(adjust extraction paths too). Development `prepare`/`native` snapshots do not
replace committed `release`. Headless builds use no default features and only
`agent-runtime`; the actual compiler host determines target.

Before shipping, require native build and extracted fixture on every shipped
target. The manual workflow covers Linux x64 and both Macs from one committed
ref. Require independent source/artifact review; an older release or different
CPU is not substitute evidence. The fixture proves actual/failed MCP inventory,
zero model calls before inventory, approval refusal, `false`/`true`/`false` local
outcomes, provider-preexecuted `null`, generation/state/close and socket mode0600.

Review binary hash/size/target, manifest/provenance binding, all checksums,
licenses/NOTICE, canonical logos and normalized archive ownership. Preserve
manifest v1; additional fork/distribution/build policy belongs in checksummed
`RELEASE.json`. Preserve fixed HOME remapping to `/build/home` and rejection of
supplied/canonical private HOME bytes. Do not inherit arbitrary compiler flags.
Restricted env is not proof that dependency processes never read HOME files.

Use `enact-agent-v` tags, separate from upstream `v*` publishing automation.
Verify uploaded archives and immutable versioned checksums through unauthenticated
downloads before claiming installation works. Do not silently reuse a stale
checksum alias, omit warnings/gaps, claim an unrun baseline compile, republish
changed artifacts under `.2`, or retag any existing release.
