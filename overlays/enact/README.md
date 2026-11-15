# Enact Agent distribution

Read [the overlay maintenance and installation contract](AGENTS.md) before
changing this fork or synchronizing upstream. It also contains the verified
three-target prebuilt installation route.

The headless distribution source fork is
[adl-tan-theta/enact-agent](https://github.com/adl-tan-theta/enact-agent).
`origin` remains `https://github.com/zeroclaw-labs/zeroclaw.git`; the `enact`
remote points to the fork. Upstream licenses, copyright notices and `NOTICE`
remain intact.

`brand.json` defines the display name, command, accent and canonical light/dark
logo files. `release.json` pins the upstream source base, fork repository,
distribution version. The compiler host determines the artifact target. The distribution tag
`enact-agent-v0.8.7-enact.1` is separate from the upstream binary version `0.8.7`.

## Apple Silicon headless release

The first verified artifact targets **aarch64-apple-darwin**. Release builds
run natively on macOS or Linux and label the archive with the actual compiler
host; another architecture is supported only after its build and fixture pass.
The release uses no default features and only `agent-runtime`; it does not
bundle a dashboard or desktop app.

From a clean, committed checkout with Python 3.11+, Cargo and Rust installed:

```sh
/usr/bin/env -i PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin \
  HOME="$HOME" LANG=en_US.UTF-8 \
  /opt/homebrew/bin/python3 -B overlays/enact/build.py release \
  --cargo /opt/homebrew/bin/cargo --rustc /opt/homebrew/bin/rustc
```

The release build uses locked, offline Cargo dependencies; these must already
be cached. It fails on a dirty source checkout or an existing output directory.
It reads source from the committed Git tree and excludes ignored web output.
Cargo receives a restricted environment without inherited provider credentials
or compiler overrides. One fixed encoded Rust flag maps the build HOME path to
`/build/home`, including HOME paths containing spaces. Release creation fails
before packaging if the resulting binary still contains the actual build HOME
path. `RELEASE.json` records this policy without recording the private path.
Archive owner IDs/names and ownership PAX fields are normalized.

Output is under `target/enact/releases/enact-agent-v0.8.7-enact.1/`:

- `enact-agent-v0.8.7-enact.1-aarch64-apple-darwin.tar.gz` and its `SHA256SUMS`.
- An unpacked `enact-agent/` directory for local qualification.

The archive contains the executable, `manifest.json`, `RELEASE.json`, both upstream licenses,
`NOTICE`, `UPSTREAM.txt`, canonical light/dark SVGs, `INSTALL.txt` and an inner
`SHA256SUMS`. Follow `INSTALL.txt` to verify and install the complete directory
under `$HOME/.local/share/enact-agent/`, with a launcher link in
`$HOME/.local/bin/`. Installation needs no root access and retains attribution.
This local release has no notarization or GitHub build attestation. Checksums
establish file integrity, not publisher identity.

The optional **Enact Agent native build** workflow runs only on manual dispatch
from a protected `enact/branding` ref. Ubuntu 22.04 x64, macOS 15 Intel and Apple
Silicon runners use the canonical workspace `rust-version`, locked dependency
prefetch, Azure Blob sccache and the offline release build. Configure the
repository secret `SCCACHE_AZURE_CONNECTION_STRING` and repository variable
`SCCACHE_AZURE_BLOB_CONTAINER` to the same cache used by Enact; set
`SCCACHE_AZURE_KEY_PREFIX` only when that shared cache uses a prefix. The
workflow fails before building when the required secret or container variable
is missing. Use a container-scoped, least-privilege SAS connection string, not
an account-wide storage key. The secret is supplied only when starting the
sccache server and is omitted from Cargo's compiler environment. Build code on
the same runner account can inspect the server process, so protect
`enact/branding` and run only trusted source there. Each job normalizes the
checkout root for cross-worktree cache keys, asserts the Azure backend is
selected, rejects cache read/write errors and reports sccache stats. Each job
tests the extracted archive against the local fixture and uploads its target
archive plus `SHA256SUMS` as workflow artifacts. It does not publish a GitHub
Release or use provider credentials. Each additional target remains unqualified
until its native job passes; no existing machine's architecture is assumed.

### Use shared sccache for a local native build

Install sccache and configure its Azure Blob backend in your existing sccache
environment. Start one server per checkout so each server receives that
worktree's normalized root while all servers share the same container and key
prefix. Restart that checkout's server after changing the backend, container or
prefix; sccache reads those settings at server startup. The build helper refuses
a missing executable when caching is enabled. It omits backend credentials from
Cargo's compiler environment, though same-user build code can inspect the
sccache server process.

```sh
repo_root="$(cd "$(git rev-parse --show-toplevel)" && pwd -P)"
socket_id="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(sys.argv[1].encode()).hexdigest()[:16])' "$repo_root")"
cache_socket="/tmp/enact-agent-sccache-$socket_id.sock"
SCCACHE_BASEDIRS="$repo_root" SCCACHE_SERVER_UDS="$cache_socket" \
  SCCACHE_IDLE_TIMEOUT=0 sccache --start-server

python_bin="$(command -v python3)"
cargo_bin="$(command -v cargo)"
rustc_bin="$(command -v rustc)"
sccache_bin="$(command -v sccache)"
env -i HOME="$HOME" PATH="$PATH" LANG=en_US.UTF-8 \
  ENACT_AGENT_USE_SCCACHE=1 RUSTC_WRAPPER="$sccache_bin" CARGO_INCREMENTAL=0 \
  SCCACHE_BASEDIRS="$repo_root" SCCACHE_SERVER_UDS="$cache_socket" SCCACHE_IDLE_TIMEOUT=0 \
  "$python_bin" -B overlays/enact/build.py release \
  --cargo "$cargo_bin" --rustc "$rustc_bin"
SCCACHE_SERVER_UDS="$cache_socket" sccache --show-stats
```

Run the `sccache --start-server` command from a shell with the existing Azure
cache configuration available. Do not pass Azure credentials to the `env -i`
build command. The worktree shares hits only when its sccache server points to
the same Azure account, container and key prefix as the other worktrees.

Qualify the unpacked executable without real provider credentials or paid calls:

```sh
python3 -B overlays/enact/probe_native.py \
  --binary target/enact/releases/enact-agent-v0.8.7-enact.1/enact-agent/enact-agent
```

The fixture exercises local RPC, constructed MCP inventory, approval rejection,
success/failure status and provider-preexecuted unknown status. The latter is
`success: null`; consumers must reject unknown status rather than infer it
from output text.

## Provenance and compatibility

`manifest.json` retains the exact version-1 shape. Its `binary.version` is the
upstream runtime version, `source.upstreamCommit` is the pinned upstream base,
and `source.overlay.sha256` fingerprints the complete prepared source after
branding. The separate, checksummed `RELEASE.json` records distribution version
and tag, fork commit, repository URLs, source dirty state and feature selection.
It binds these facts to `manifest.json` through `manifestSha256`.

Tracked upstream Rust files are unchanged. `build.py` projects changes into
`target/enact/source/`, with checked anchors that fail on upstream drift.
Native environment overrides use `ENACT_AGENT_*`, including
`ENACT_AGENT_CONFIG_DIR`, `ENACT_AGENT_SOCKET` and schema overrides such as
`ENACT_AGENT_runtime_profiles__probe__max_tool_iterations`.
The native protocol is `enact-agent-local-rpc`, numeric version `1`, over Unix
NDJSON. Crate/module identifiers, existing configuration paths and upstream
attribution remain upstream-compatible. Saved user workspaces are not migrated.

## Development and optional desktop builds

`prepare` stages local working-tree changes. `native` builds a development
snapshot with the same headless feature set. `build` and `package` use the full
default feature set and require the normal web prerequisites:

```sh
npm --prefix web ci
cargo web gen-api
npm --prefix web run build
python3 overlays/enact/build.py package
```

The full package retains its internal `bin/zeroclaw` kernel and public
`bin/enact-agent` launcher. `desktop --debug` additionally needs macOS `sips`
and `iconutil`; its canonical SVGs preserve aspect ratio and follow splash
color preference. Desktop bundles use an ad-hoc signature, with no notarization.
These optional distribution paths are separate from the headless release.
Builds do not install apps, modify launch agents or stop existing services.

## Upstream updates

Keep overlay changes on `enact/branding`. Fetch and merge `origin/master`,
review overlay-anchor failures and update the release base/version deliberately.
Rebuild and qualify before replacing an installation. The branded updater
blocks upstream binary downloads, which would erase this overlay. Restore an
upstream build through the upstream branch and documented installation workflow;
preserve private configuration and data when changing installations.
