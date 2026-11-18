//! Architecture gate for the fork's release workflow and installer contract.
//!
//! The fork publishes its own distribution: `enact-agent-v<version>` tags and
//! `<tag>-<target>.tar.gz` archives containing an `enact-agent` binary.
//! `install.sh` resolves the newest published release and downloads exactly
//! those names, so the workflow's asset names, the checksum file name and the
//! installer's resolution are one contract split across two files.
//!
//! Nothing here is cosmetic. A renamed asset makes the installer fall back
//! to a source build; a release published without `--latest` is invisible to
//! the installer entirely, because GitHub's Releases "latest" endpoint omits
//! pre-releases. These gates keep the two sides pinned to each other.

use std::fs;
use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

fn release_workflow() -> String {
    read(".github/workflows/enact-agent-release.yml")
}

fn installer() -> String {
    read("install.sh")
}

fn release_version() -> String {
    let manifest = read("overlays/enact/release.json");
    manifest
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("\"version\":")
                .map(|rest| rest.trim().trim_end_matches(',').trim_matches('"'))
                .map(str::to_string)
        })
        .expect("overlays/enact/release.json must record a version")
}

#[test]
fn the_release_workflow_triggers_on_the_fork_tag_scheme() {
    let workflow = release_workflow();

    assert!(
        workflow.contains("- \"enact-agent-v*\""),
        "the release workflow must trigger on the fork's own `enact-agent-v*` tags: \
         the upstream `v*` tags belong to ZeroClaw and must never publish this fork"
    );
    assert!(
        !workflow.contains("- \"v[0-9]"),
        "the release workflow must not trigger on upstream `v*` tags"
    );
}

#[test]
fn the_release_workflow_publishes_assets_the_installer_can_find() {
    let workflow = release_workflow();

    // The archive name and the checksum file name are the two names the
    // installer builds its download URLs from. They are spelled out in
    // `install.sh`; this gate keeps the publisher on the same spelling.
    assert!(
        workflow.contains("\"release-assets/SHA256SUMS-${RELEASE_VERSION}\""),
        "the release must publish SHA256SUMS-<version>, the name install.sh fetches"
    );
    assert!(
        workflow.contains("release-assets/SHA256SUMS"),
        "the release must also publish the plain SHA256SUMS the overlay writes"
    );

    // Every published archive is named after the distribution tag. The tag is
    // derived from the recorded version, so the asset name and the tag can
    // never drift apart.
    assert!(
        workflow.contains("\"${RELEASE_TAG}-\"*.tar.gz"),
        "published archives must be named <tag>-<target>.tar.gz"
    );
}

#[test]
fn the_release_workflow_publishes_the_distribution_as_latest() {
    let workflow = release_workflow();

    // GitHub's Releases "latest" endpoint reports only non-prerelease,
    // non-draft releases. A release published without `--latest` never
    // reaches the installer, which resolves the distribution through that
    // endpoint. Every Enact Agent release published so far is a pre-release,
    // so this is the difference between a working installer and a silent
    // fallback to a source build.
    assert!(
        workflow.contains("--latest"),
        "the release must be published with --latest, or install.sh cannot find it"
    );
}

#[test]
fn the_release_workflow_builds_every_target_the_installer_resolves() {
    let workflow = release_workflow();

    for target in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ] {
        assert!(
            workflow.contains(target),
            "the release must ship {target}: install.sh resolves this target and \
             falls back to a source build when no asset is published for it"
        );
    }
}

#[test]
fn the_release_workflow_validates_the_recorded_distribution_version() {
    let workflow = release_workflow();
    let version = release_version();

    assert!(
        version.contains("-enact."),
        "the recorded distribution version must carry the -enact.N suffix, got {version:?}"
    );

    // The workflow reads the version from the same file the overlay packages
    // from, so the tag, the asset names and the binary's own version cannot
    // disagree.
    assert!(
        workflow.contains("overlays/enact/release.json"),
        "the release workflow must derive its version from overlays/enact/release.json"
    );
    assert!(
        workflow.contains("enact-agent-v${version}"),
        "the distribution tag must be derived from the recorded version"
    );
}

#[test]
fn the_installer_resolves_a_release_through_the_releases_list() {
    let installer = installer();

    // The "latest" endpoint omits pre-releases, and every Enact Agent
    // release so far is one, so it answers 404 and the installer can never
    // find a prebuilt binary. The releases list is the authority for
    // "newest" and reports pre-releases.
    assert!(
        installer.contains("/releases?per_page=100"),
        "the installer must read the releases list: the \"latest\" endpoint omits \
         pre-releases and answers 404 for this fork's entire release history"
    );
    assert!(
        !installer.contains("/releases/latest"),
        "the installer must not resolve the release through the \"latest\" endpoint"
    );
}

#[test]
fn the_installer_and_the_release_agree_on_the_checksum_file_name() {
    let installer = installer();

    // The installer derives the checksum file name from the tag it resolved.
    assert!(
        installer.contains("SHA256SUMS-${tag#enact-agent-v}"),
        "the installer must fetch SHA256SUMS-<version>, the name the release publishes"
    );
}

#[test]
fn the_installer_resolves_the_distribution_tag_and_nothing_else() {
    let installer = installer();

    // The resolution must yield a bare tag. A pattern that fails to match
    // would otherwise emit the whole API payload, and the installer would
    // build a download URL out of a JSON document.
    assert!(
        installer.contains("sed -n 's/.*\"tag_name\"[[:space:]]*:[[:space:]]*\"\\(enact-agent-v[^\"]*\\)\".*/\\1/p'"),
        "the resolution must print the tag and nothing else: a pattern that does \
         not match must produce an empty result, not the raw payload"
    );
}

#[test]
fn the_release_workflow_and_the_installer_share_the_distribution_tag_prefix() {
    let workflow = release_workflow();
    let installer = installer();

    assert!(
        workflow.contains("enact-agent-v"),
        "the release workflow must build distribution tags with the enact-agent-v prefix"
    );
    assert!(
        installer.contains("enact-agent-v"),
        "the installer must resolve distribution tags with the enact-agent-v prefix"
    );
}
