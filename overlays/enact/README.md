# Enact Agent distribution

The headless distribution source fork is
[adl-tan-theta/enact-agent](https://github.com/adl-tan-theta/enact-agent).
`origin` remains `https://github.com/zeroclaw-labs/zeroclaw.git`; the `enact`
remote points to the fork. Upstream licenses, copyright notices and `NOTICE`
remain intact.

`brand.json` defines the display name, command, accent and canonical light/dark
logo files. `release.json` pins the upstream source base, fork repository,
distribution version. The compiler host determines the artifact target. The distribution tag
`enact-agent-v0.8.6-enact.1` is separate from the upstream binary version `0.8.6`.

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
or compiler overrides.

Output is under `target/enact/releases/enact-agent-v0.8.6-enact.1/`:

- `enact-agent-v0.8.6-enact.1-aarch64-apple-darwin.tar.gz` and its `SHA256SUMS`.
- An unpacked `enact-agent/` directory for local qualification.

The archive contains the executable, `manifest.json`, `RELEASE.json`, both upstream licenses,
`NOTICE`, `UPSTREAM.txt`, canonical light/dark SVGs, `INSTALL.txt` and an inner
`SHA256SUMS`. Follow `INSTALL.txt` to verify and install the complete directory
under `$HOME/.local/share/enact-agent/`, with a launcher link in
`$HOME/.local/bin/`. Installation needs no root access and retains attribution.
This local release has no notarization or GitHub build attestation. Checksums
establish file integrity, not publisher identity.

The optional **Enact Agent native build** workflow runs only on manual dispatch
from a committed fork ref. Ubuntu 22.04 x64 and macOS 15 Intel runners use
Rust 1.95.0, locked dependency prefetch and the offline release build. Each job
tests the extracted archive against the local fixture and uploads its target
archive plus `SHA256SUMS` as workflow artifacts. It does not publish a GitHub
Release or use provider credentials. Each additional target remains unqualified
until its native job passes; no existing machine's architecture is assumed.

Qualify the unpacked executable without real provider credentials or paid calls:

```sh
python3 -B overlays/enact/probe_native.py \
  --binary target/enact/releases/enact-agent-v0.8.6-enact.1/enact-agent/enact-agent
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
