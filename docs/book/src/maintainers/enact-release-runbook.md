# Enact Agent Release Runbook

This is the runbook for cutting an **Enact Agent** release from this fork.

It is not the upstream ZeroClaw release procedure. Upstream ships `vX.Y.Z`
tags, `zeroclaw` binaries, crates.io, Docker, Scoop, AUR, a website redeploy
and a signed desktop app. This fork ships none of those. It publishes a
headless native binary under its own tag scheme, and nothing else.

For upstream's process, see the inherited
[Release runbook](./release-runbook.md).

## The release contract

Four names are load-bearing. The publisher writes them, the installer reads
them, and an architecture gate
(`tests/architecture/enact_release.rs`) keeps the two sides pinned together.

| Name | Shape | Written by | Read by |
| --- | --- | --- | --- |
| Distribution tag | `enact-agent-v<version>` | `enact-agent-release.yml` | `install.sh` |
| Archive | `<tag>-<target>.tar.gz` | `build.py` via the overlay | `install.sh` |
| Checksum manifest | `SHA256SUMS-<version>` | `enact-agent-release.yml` | `install.sh` |
| Binary in the archive | `enact-agent` | `build.py` via the overlay | `install.sh`, installed as `zeroclaw` |

`<version>` is `X.Y.Z-enact.N`. The `X.Y.Z` part must equal the workspace
version in `Cargo.toml`, because the packaged binary reports that version and
`build.py` refuses to package a release whose base disagrees with it.

The binary is *named* `enact-agent` inside the archive and *installed* as
`zeroclaw`. That is deliberate: the release archive names the binary after the
distribution, while the installer keeps its canonical command name, so the
prebuilt path and the source-build path land the same file in
`$CARGO_HOME/bin`.

## The process in four steps

1. [Bump the recorded version](#step-1-bump-the-recorded-version)
2. [Tag the commit](#step-2-tag-the-commit)
3. [Watch the release workflow](#step-3-watch-the-release-workflow)
4. [Verify the published release](#step-4-verify-the-published-release)

### Step 1: Bump the recorded version

Edit `overlays/enact/release.json`:

```json
{
  "version": "0.9.0-enact.5",
  "upstreamRepository": "https://github.com/zeroclaw-labs/zeroclaw.git",
  "forkRepository": "https://github.com/adl-tan-theta/enact-agent.git",
  "upstreamCommit": "3a3f2e35865579843e5c37d8eee2e612cd98cd26"
}
```

The version must use `X.Y.Z-enact.N`; `build.py` rejects any other shape. The
`X.Y.Z` base must equal the workspace version in `Cargo.toml`. Bump both in
the same commit.

Leave `upstreamCommit` alone unless you are deliberately rebasing onto a new
upstream base. It is the recorded upstream provenance, and the release build
verifies that it is an ancestor of the commit being released.

Merge the version bump through the normal pull-request path. The required
checks are the same ones every other change has to pass.

### Step 2: Tag the commit

```sh
git tag enact-agent-v0.9.0-enact.5 <commit>
git push origin enact-agent-v0.9.0-enact.5
```

The tag name must match the recorded version exactly. The workflow refuses a
tag that disagrees with `overlays/enact/release.json`, because a mismatch
would publish assets under a name the installer does not look for.

Tag the merged commit on `enact/branding`, not a local branch head.

### Step 3: Watch the release workflow

`enact-agent-release.yml` runs on the tag push. It has three jobs:

| Job | What it does |
| --- | --- |
| `Validate Release Version` | Checks the tag against the recorded version and the `Cargo.toml` base |
| `Build <os>` | Builds the native binary on Linux x86_64, macOS Intel and macOS Apple Silicon, then packages and probes it |
| `Publish Release` | Merges the per-platform checksums, publishes the release and replays the installer's resolution against it |

The build job is the slow one. It is a full workspace compile plus a
component probe: roughly 20 minutes on the Linux runner and 45 to 80 minutes
on the macOS runners, which are often oversubscribed.

The publish job ends by resolving the release through the same Releases
endpoint the installer uses and checking that every platform's asset is
present. A release the installer cannot consume fails the run rather than
reaching a user's terminal.

### Step 4: Verify the published release

```sh
tag=enact-agent-v0.9.0-enact.5
base="https://github.com/adl-tan-theta/enact-agent/releases/download/${tag}"

curl -fsSLO "${base}/SHA256SUMS-${tag#enact-agent-v}"
curl -fsSLO "${base}/${tag}-x86_64-unknown-linux-gnu.tar.gz"
sha256sum -c "SHA256SUMS-${tag#enact-agent-v}"
```

Then check that the installer resolves it:

```sh
curl -fsSL "https://api.github.com/repos/adl-tan-theta/enact-agent/releases?per_page=100" |
  grep '"tag_name"' | head -1
```

## Two things that will silently break the installer

**A release published as a pre-release.** GitHub's Releases `latest` endpoint
reports only non-prerelease, non-draft releases, so it answers `404` when the
newest publication is a pre-release. The installer resolves the release
through the releases *list*, which does report pre-releases, and the workflow
publishes with `--latest` so the release is also the one GitHub considers
current. Both halves matter: the list keeps the installer working while a
pre-release is newest, and `--latest` keeps the release visible to anything
else that asks GitHub for the current release.

**A draft.** The releases list reports drafts alongside published releases. A
draft is not yet a distribution and its assets may still be incomplete, so the
installer skips it.

## What the release does not do

There is no crates.io publish, no Docker image, no signed desktop app, no
Scoop bucket, no AUR package, no website redeploy and no announcement post.
The upstream release workflow (`release-stable-manual.yml`) still exists in
this fork and still describes all of those, but it is wired to upstream's
`v*` tags and upstream's `zeroclaw-labs` repository. It does not publish this
fork's distribution and should not be dispatched for it.

## Reproducing a release build locally

```sh
python3 -B overlays/enact/build.py prepare
python3 -B overlays/enact/build.py release --output-dir /tmp/enact-release
```

The `release` action requires a clean committed worktree and an `origin`
remote pointing at the upstream repository, because it treats upstream as the
source of the prepared tree. The release workflow satisfies this by renaming
the fork's remote and adding upstream in its place. Locally, do the same, or
expect the provenance check to refuse.

The build is host-native: it produces an archive for the machine it runs on.
The three published targets come from three different runners.
