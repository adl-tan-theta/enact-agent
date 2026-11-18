//! Architecture gate for Quality Gate and Advisory Windows Tests runner selection.
//!
//! The compile-heavy jobs name their runner label directly instead of reading
//! it from a `fmt` output. `runs-on` resolves before a job is created, so
//! reading it from another job forced every compile job to wait for a
//! GitHub-hosted runner to pick up the formatting check first. Writing the
//! constant once per job is what removes that wait, and this gate is what keeps
//! the copies honest: a stray label sends one job to a different runner class,
//! and a stray cache-provider input sends it to a cache the rest of the fleet
//! never writes.
//!
//! This fork runs the whole fleet on GitHub-hosted runners. Upstream pins it to
//! the Blacksmith fleet, whose labels only resolve where the Blacksmith GitHub
//! App is installed on an organisation. This fork lives under a user account,
//! which has no organisation level to inherit an app installation from, so
//! every label of that form had no runner behind it. A label with no runner
//! does not fail — it waits forever. The Quality Gate therefore created zero
//! jobs in this fork and reported `pending` indefinitely, which is how three
//! real lint failures stayed invisible until they were found by running the
//! gate's jobs locally. The inventory below is what keeps that class of
//! mistake reviewable in one place.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;

/// The one runner label the fleet may use. Moving the fleet means changing this
/// constant and every job below in the same reviewed commit.
///
/// This fork runs on GitHub-hosted runners. Upstream pins the fleet to
/// Blacksmith's `blacksmith-8vcpu-ubuntu-2404`, a label that only resolves
/// where the Blacksmith GitHub App is installed on an organisation. This fork
/// lives under a user account with no organisation level, so that label had no
/// runner behind it and the Quality Gate created zero jobs, forever, without
/// failing. `ubuntu-24.04` is the GitHub-hosted image with the same Ubuntu
/// version upstream's fleet compiled on.
const RUNNER_LABEL: &str = "ubuntu-24.04";

/// Every job in the two workflows that runs on the fleet label.
///
/// Upstream splits the fleet in two classes — Blacksmith 8-vCPU for the
/// compile-heavy jobs, 4-vCPU for the housekeeping jobs — so a single label
/// cannot carry two mutually exclusive inventories. In this fork both classes
/// are the same GitHub-hosted image, so there is one label and one inventory.
/// The compile/housekeeping *roles* are still listed separately below,
/// because the roles are what the reviewer is checking: the compile jobs are
/// the ones whose runner size matters, and the housekeeping jobs are the ones
/// whose work is minutes.
///
/// Workflow-qualified IDs keep same-named jobs in different workflows distinct.
const FLEET_JOBS: [&str; 32] = [
    // Compile-heavy: these need the machine.
    "ci.yml/lint",
    "ci.yml/build",
    "ci.yml/check",
    "ci.yml/check-plugin-backends",
    "ci.yml/msrv",
    "ci.yml/check-32bit",
    "ci.yml/bench",
    "ci.yml/test",
    "ci.yml/test-channel-features",
    "ci.yml/test-tool-features",
    "ci.yml/test-cli-only",
    "ci.yml/memory-postgres-test",
    "ci.yml/parallel-runtime-test",
    "ci.yml/installer-drift",
    // Housekeeping: formatting, change detection, repository guards, docs and
    // policy gates, the Nix checks, the container smoke, the Windows-test
    // selector, and the required-gate aggregator.
    "ci.yml/fmt",
    "ci.yml/crates-preflight-changes",
    "ci.yml/master-debounce",
    "ci.yml/gate",
    "ci.yml/history-guard",
    "ci.yml/repo-structure",
    "ci.yml/docs-style",
    "ci.yml/zerocode-rpc-boundary",
    "ci.yml/parallel-runtime-test-changes",
    "ci.yml/path-changes",
    "ci.yml/relay-container-smoke-changes",
    "ci.yml/nix-eval",
    "ci.yml/nix-hash-drift",
    "ci.yml/relay-container-smoke",
    "ci.yml/security",
    "ci.yml/web-permission-tests",
    "windows-tests.yml/windows-test-scope",
    // A reusable-workflow caller: it has no `runs-on` of its own and hands the
    // label to the called workflow.
    "ci.yml/crates-preflight",
];

/// Jobs that stay on GitHub-hosted `ubuntu-latest` because they depend on the
/// hosted image itself. `test-landlock` exercises the Landlock LSM, which the
/// GitHub kernel enables; a different runner class could silently skip that
/// security coverage. Adding a job here requires a reason of that kind, not
/// convenience: during a hosted outage, every entry in this list is a check
/// that cannot run.
///
/// This fork's whole fleet is GitHub-hosted, so this allowlist is the
/// exception list rather than the rule: `ubuntu-latest` is the hosted label
/// and `ubuntu-24.04` is the fleet label, and a job may only sit on the
/// former for the Landlock reason above.
const HOSTED_LINUX_JOBS: [&str; 1] = ["test-landlock"];

/// Every job that compiles the workspace. A new compile job must be added
/// here, which is the point: the list is the inventory this gate checks the
/// workflow against.
const COMPILE_JOBS: [&str; 14] = [
    "lint",
    "build",
    "check",
    "check-plugin-backends",
    "msrv",
    "check-32bit",
    "bench",
    "test",
    "test-channel-features",
    "test-tool-features",
    "test-cli-only",
    "memory-postgres-test",
    "parallel-runtime-test",
    "installer-drift",
];

/// Compile jobs that call a reusable workflow and hand it the fleet label as
/// its `runner` input. They have no `runs-on` of their own, so they are kept
/// apart from COMPILE_JOBS but still count toward the fleet inventory.
const REUSABLE_COMPILE_JOBS: [&str; 1] = ["crates-preflight"];

/// `use-blacksmith` inputs the rust-cache composite may receive. The composite
/// treats anything other than `'true'` as "use Swatinem/rust-cache", so a
/// GitHub-hosted job must pass `'false'`.
///
/// This fork has no Blacksmith app, so no job may pass `'true'`: the
/// Blacksmith cache backend needs the local sticky disk that only a
/// Blacksmith runner provides, and a hosted runner has none. A job that
/// passed `'true'` would silently restore no cache at all.
const ALLOWED_CACHE_INPUTS: [&str; 1] = ["'false'"];

fn ci_workflow() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(".github/workflows/ci.yml"))
        .expect("failed to read .github/workflows/ci.yml")
}

fn runner_workflow_jobs() -> BTreeMap<String, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    ["ci.yml", "windows-tests.yml"]
        .into_iter()
        .flat_map(|filename| {
            let workflow = fs::read_to_string(root.join(filename))
                .unwrap_or_else(|error| panic!("failed to read {filename}: {error}"));
            job_blocks(&workflow)
                .into_iter()
                .map(move |(name, block)| (format!("{filename}/{name}"), block))
        })
        .collect()
}

/// Split the `jobs:` mapping into `job id -> job body`. Only scans after the
/// top-level `jobs:` key so that `on:` children such as `pull_request:` are
/// never mistaken for jobs.
fn job_blocks(workflow: &str) -> BTreeMap<String, String> {
    let (_, jobs) = workflow
        .split_once("\njobs:\n")
        .expect("workflow must declare a top-level jobs mapping");
    let header = Regex::new(r"(?m)^  ([a-z0-9-]+):$").expect("valid job-header pattern");

    let starts: Vec<(usize, String)> = header
        .captures_iter(jobs)
        .map(|capture| {
            let whole = capture.get(0).expect("match 0 always exists");
            (whole.start(), capture[1].to_string())
        })
        .collect();
    assert!(!starts.is_empty(), "workflow must define at least one job");

    let mut blocks = BTreeMap::new();
    for (index, (offset, name)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(jobs.len(), |(next, _)| *next);
        let previous = blocks.insert(name.clone(), jobs[*offset..end].to_string());
        assert!(previous.is_none(), "duplicate job id {name} in workflow");
    }
    blocks
}

/// The `needs:` list of a job, empty when it declares none.
fn needs(block: &str) -> Vec<String> {
    let Some(line) = block.lines().find(|line| line.starts_with("    needs: [")) else {
        return Vec::new();
    };
    line.trim_start()
        .trim_start_matches("needs: [")
        .trim_end_matches(']')
        .split(',')
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect()
}

#[test]
fn compile_jobs_pin_the_runner_label_instead_of_reading_it_from_fmt() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    assert!(
        !workflow.contains("needs.fmt.outputs"),
        "no job may read a runner label or cache provider from fmt: that forces \
         every compile job to wait for a GitHub-hosted runner before it starts"
    );

    for name in COMPILE_JOBS {
        let block = blocks
            .get(name)
            .unwrap_or_else(|| panic!("ci.yml must define the {name} job"));
        assert!(
            !needs(block).iter().any(|dependency| dependency == "fmt"),
            "{name} must not declare needs: [fmt]; the gate job is what keeps a \
             formatting error blocking merge"
        );
        let pins_label = if name == "build" {
            // The Linux leg carries the label through the matrix; macOS names
            // its GitHub-hosted image. Windows is selected independently.
            block.contains(&format!("- os: {RUNNER_LABEL}\n"))
                && block.contains("runs-on: ${{ matrix.os }}\n")
        } else {
            block.contains(&format!("    runs-on: {RUNNER_LABEL}\n"))
        };
        assert!(pins_label, "{name} must run on {RUNNER_LABEL}");
    }
}

#[test]
fn only_the_declared_fleet_jobs_claim_the_fleet_label() {
    let blocks = runner_workflow_jobs();

    let claiming: BTreeSet<String> = blocks
        .iter()
        .filter(|(_, block)| block.contains(RUNNER_LABEL))
        .map(|(name, _)| name.clone())
        .collect();
    let declared: BTreeSet<String> = FLEET_JOBS.into_iter().map(String::from).collect();

    assert_eq!(
        claiming, declared,
        "every job using {RUNNER_LABEL} must be listed in FLEET_JOBS, so the fleet \
         inventory stays reviewable in one place. A job that is added to the \
         workflow without being listed here would otherwise reach the runner \
         unreviewed — and a job that leaves the inventory would silently stop \
         claiming the fleet label."
    );
}

#[test]
fn the_fleet_inventory_covers_every_compile_and_reusable_job() {
    // The compile fleet and the reusable-workflow jobs are the ones whose
    // runner class decides whether a merge is safe. They must all be in the
    // single fleet inventory, and the reusable ones have no `runs-on` of their
    // own — they hand the label to the called workflow instead.
    let fleet: BTreeSet<&str> = FLEET_JOBS.into_iter().collect();

    for name in COMPILE_JOBS {
        assert!(
            fleet.contains(&format!("ci.yml/{name}").as_str()),
            "the compile job {name} must be in FLEET_JOBS"
        );
    }
    for name in REUSABLE_COMPILE_JOBS {
        assert!(
            fleet.contains(&format!("ci.yml/{name}").as_str()),
            "the reusable compile job {name} must be in FLEET_JOBS"
        );
    }
}

#[test]
fn rust_cache_callers_pass_a_reviewed_provider_input() {
    let workflow = ci_workflow();

    for line in workflow.lines() {
        let trimmed = line.trim_start();
        // A comment may mention the input by name; only a real `with:` entry
        // sets the provider.
        if trimmed.starts_with('#') {
            continue;
        }
        let Some((_, value)) = trimmed.split_once("use-blacksmith: ") else {
            continue;
        };
        assert!(
            ALLOWED_CACHE_INPUTS.contains(&value.trim()),
            "unexpected rust-cache provider input {value:?}: a job that writes a \
             cache the rest of the fleet never reads silently loses its cache"
        );
    }
}

#[test]
fn the_required_gate_carries_no_windows_runner() {
    // The Quality Gate is Mac + Linux only. The Windows legs that used to sit
    // in it were removed so the required gate does not spend its runner
    // budget on a platform this fork does not ship. `windows-tests.yml` is a
    // separate workflow and stays where it is.
    let blocks = runner_workflow_jobs();
    let windows_label =
        Regex::new(r"(?m)^\s*(?:runs-on|os):.*windows").expect("valid runner-label pattern");
    let claiming: BTreeSet<&str> = blocks
        .iter()
        .filter(|(name, _)| name.starts_with("ci.yml/"))
        .filter(|(_, block)| windows_label.is_match(block))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        claiming,
        BTreeSet::new(),
        "the required gate must not run a Windows job: it is Mac + Linux only"
    );

    let ci = ci_workflow();
    assert!(
        !ci.contains("windows-latest") && !ci.contains("windows-2025"),
        "the Quality Gate workflow must not name a Windows runner at all"
    );
    for removed in [
        "windows-build",
        "windows-clippy-tools",
        "windows-clippy-tools-changes",
        "windows-required-changes",
        "windows-service-smoke",
        "windows-task-owner-recovery",
    ] {
        assert!(
            !ci.contains(&format!("  {removed}:")),
            "the Windows job {removed} must not be back in the required gate"
        );
    }
}

#[test]
fn every_fleet_job_pins_the_fleet_label() {
    let blocks = runner_workflow_jobs();

    for name in FLEET_JOBS {
        let block = blocks
            .get(name)
            .unwrap_or_else(|| panic!("missing workflow job {name}"));
        assert!(
            block.contains(RUNNER_LABEL),
            "{name} must run on {RUNNER_LABEL}"
        );
    }
}

#[test]
fn no_job_names_a_runner_label_that_has_no_runner_behind_it() {
    // A `runs-on` label with no runner behind it does not fail: the job waits
    // forever. That is how this fork's Quality Gate created zero jobs on every
    // push and reported `pending` indefinitely, which hid three real lint
    // failures. A provider-specific label is the trap: it only resolves where
    // that provider's GitHub App is installed on an organisation, and this fork
    // lives under a user account that has no organisation level to inherit an
    // installation from.
    let blocks = runner_workflow_jobs();

    for (name, block) in &blocks {
        for line in block.lines() {
            let trimmed = line.trim();
            let Some(value) = trimmed
                .strip_prefix("runs-on: ")
                .or_else(|| trimmed.strip_prefix("- os: "))
                .or_else(|| trimmed.strip_prefix("runner: "))
            else {
                continue;
            };
            let value = value.trim();
            if value.starts_with("${{") {
                // A matrix expression: the concrete labels are checked
                // separately by the inventory tests above.
                continue;
            }
            assert!(
                !value.starts_with("blacksmith-"),
                "{name} names the runner label {value:?}, which only resolves where \
                 the Blacksmith GitHub App is installed on an organisation. This \
                 fork has no such installation, so the job would queue forever \
                 without failing."
            );
        }
    }
}

#[test]
fn hosted_linux_stays_an_explicit_allowlist() {
    let blocks = runner_workflow_jobs();

    let hosted: BTreeSet<String> = blocks
        .iter()
        .filter(|(_, block)| block.contains("    runs-on: ubuntu-latest\n"))
        .map(|(name, _)| name.clone())
        .collect();
    let declared: BTreeSet<String> = HOSTED_LINUX_JOBS
        .into_iter()
        .map(|name| format!("ci.yml/{name}"))
        .collect();

    assert_eq!(
        hosted, declared,
        "a Linux job may use GitHub-hosted ubuntu-latest only when it depends \
         on the hosted image itself (see HOSTED_LINUX_JOBS): every job here is \
         one these workflows cannot run during a hosted-runner outage"
    );
}

#[test]
fn the_required_gate_still_waits_for_formatting() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);
    let gate = blocks.get("gate").expect("ci.yml must define the gate job");

    assert!(
        needs(gate).iter().any(|dependency| dependency == "fmt"),
        "CI Required Gate must keep needing fmt: it is the only thing that still \
         makes a formatting error block merge"
    );
    for name in ["windows-required-changes", "windows-build"] {
        assert!(
            !needs(gate).iter().any(|dependency| dependency == name),
            "the required gate is Mac + Linux only and must not consume {name}"
        );
    }
    let build = blocks.get("build").expect("ci.yml must define build");
    assert!(
        !needs(build)
            .iter()
            .any(|dependency| dependency == "windows-required-changes"),
        "Linux/macOS compilation must not wait for Windows selection"
    );
}

#[test]
fn reusable_compile_jobs_pass_the_fleet_label() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    // These jobs compile the workspace and block the required gate. Without
    // an explicit `runner`, the called workflow falls back to GitHub-hosted
    // `ubuntu-latest`, which a hosted-runner outage would strand.
    for name in REUSABLE_COMPILE_JOBS {
        let block = blocks
            .get(name)
            .unwrap_or_else(|| panic!("ci.yml must define the {name} job"));
        assert!(
            block.contains("    uses: ./.github/workflows/")
                && block.contains(&format!("      runner: {RUNNER_LABEL}\n")),
            "{name} must pass runner: {RUNNER_LABEL} to its reusable workflow"
        );
    }
}

/// The job-level `if:` expression of a job, empty when it declares none.
fn job_if(block: &str) -> String {
    block
        .lines()
        .find(|line| line.starts_with("    if: "))
        .map(|line| line.trim_start().trim_start_matches("if: ").to_string())
        .unwrap_or_default()
}

/// Master pushes wait in `master-debounce` so a superseded push is cancelled
/// before its compile fleet starts. That job is skipped on pull_request and
/// merge_group, and GitHub's implicit `success()` propagates a skipped
/// ancestor transitively, so any job downstream of it that lacks an explicit
/// status function would silently skip on every PR while `CI Required Gate`
/// still reported green. This guard keeps every downstream job explicit.
#[test]
fn jobs_downstream_of_the_master_debounce_never_skip_silently() {
    let workflow = ci_workflow();
    let blocks = job_blocks(&workflow);

    let debounce = blocks
        .get("master-debounce")
        .expect("ci.yml must define the master-debounce job");
    assert_eq!(
        job_if(debounce),
        "github.event_name == 'push'",
        "master-debounce must run only on master pushes, never on PRs or the merge queue"
    );

    let gate = blocks.get("gate").expect("ci.yml must define the gate job");
    assert!(
        needs(gate)
            .iter()
            .any(|dependency| dependency == "master-debounce"),
        "CI Required Gate must need master-debounce, so a debounce failure cannot \
         leave skipped compile jobs behind a green gate"
    );

    let mut downstream: BTreeSet<String> = BTreeSet::from(["master-debounce".to_string()]);
    loop {
        let before = downstream.len();
        for (name, block) in &blocks {
            if needs(block)
                .iter()
                .any(|dependency| downstream.contains(dependency))
            {
                downstream.insert(name.clone());
            }
        }
        if downstream.len() == before {
            break;
        }
    }
    downstream.remove("master-debounce");
    downstream.remove("gate");
    assert!(
        downstream.len() > 10,
        "the compile fleet must wait on master-debounce; found only {downstream:?}"
    );

    for name in &downstream {
        let block = &blocks[name];
        let condition = job_if(block);
        assert!(
            condition.contains("!cancelled()"),
            "{name} is downstream of the push-only master-debounce and must use an \
             explicit `!cancelled()` condition; an implicit success() skips it on PRs"
        );
        for dependency in needs(block) {
            let expected = if dependency == "master-debounce" {
                "needs.master-debounce.result != 'failure'".to_string()
            } else {
                format!("needs.{dependency}.result == 'success'")
            };
            assert!(
                condition.contains(&expected),
                "{name} must check `{expected}` so replacing implicit success() does not \
                 let it run after a failed dependency"
            );
        }
    }
}
