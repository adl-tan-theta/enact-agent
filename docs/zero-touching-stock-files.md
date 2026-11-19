# Scope: zero touching of stock files

**Goal:** every file `origin/master` owns is byte-identical to upstream in
`enact/branding`. Zero exceptions. The fork's text lives only in
`overlays/enact/` and in the ephemeral prepared tree.

**Acceptance criterion:**
```sh
git diff --exit-code "$(python3 -c 'import json;print(json.load(open("overlays/enact/release.json"))["upstreamCommit"])')" -- . ':(exclude)overlays/'
```
must be empty.

## Facts established by measurement

| Fact | Evidence |
| --- | --- |
| The overlay store feeds `target/enact/source/` only. | `build.py` `prepare()` |
| `xtask`'s `repo_root()` is `CARGO_MANIFEST_DIR/..`, a **compile-time** constant. | `xtask/src/util.rs:4` |
| So a `cargo web` run **from the prepared tree** targets the prepared tree. **No stock `xtask` change is needed.** | verified: prepared `xtask` is byte-identical to tracked |
| The web build is a manual step: `npm --prefix web ci && cargo web gen-api && npm --prefix web run build`. | `overlays/enact/README.md:157` |
| CI only creates a `web/dist` **placeholder**. The real web bundle is a release-time local build. | `ci.yml:362` `mkdir -p web/dist && touch web/dist/.gitkeep` |
| `build.py` copies the tracked `web/dist` into the prepared tree. | `build.py:8129` |
| Stock `deny.toml` **fails** on the fork's dependency set: 9 advisories (`chrono`, `chrono-tz`, `zip 9.0.3`, `lettre 0.11.22` ×6). All are **upstream's own pins**. | `cargo deny --config <stock deny.toml> check` → FAILED |
| A fork-owned `deny.toml` config makes it **pass**: `advisories ok, bans ok, licenses ok, sources ok`. | verified with `cargo deny --config` |
| `cargo deny --config <path>` exists. | `cargo deny --help` |
| `tests/component/dockerignore_test.rs` only asserts `deny.toml` is in the dockerignore list; it never reads its content. | verified |
| Upstream's `ci.yml` hardcodes `blacksmith-*` in **52** places. `runs-on:` resolves before any step, so no action or env var can override it. | `git show <base>:.github/workflows/ci.yml` |
| 5 stock workflows reference Blacksmith: `ci.yml` (52), `codeql.yml`, `docker-image-pr.yml`, `release-stable-manual.yml`, `windows-tests.yml`. | measured |
| Upstream's `ci.yml` is not `workflow_call`-able. | measured |
| `install.sh` is validated against the **stock** generator by `cargo generate installers --check`, which runs from the tracked tree. | `xtask/src/generate/mod.rs:257` |
| The fork's `install.sh` keeps all 12 zone markers and the `BIN="$CARGO_HOME/bin/zeroclaw"` anchor. | measured |

## The central idea

Two mechanisms, and the choice between them is per-file:

1. **Projection** — an anchored `replace_required` in `build.py` that edits
   the prepared tree. The tracked file stays stock. Use it when the fork's
   version is upstream's version plus an identifiable change.
2. **Overlay store** — `overlays/enact/files/<path>` holds the fork's whole
   file; `prepare()` copies it over the prepared tree. Use it when the fork's
   version is a genuinely different file.
3. **Fork-owned twin** — a **new** file the fork owns (not in upstream), with
   the fork's content, at a different path. Use it when the stock file's
   *content* must differ but the stock path must stay stock. This is the
   mechanism that unblocks workflows and CI scripts.

## Plan by area

### A. `web/` — 16 stock files → PROJECTION + fork-owned sources

Every web delta is a small branded-string substitution. The fork's own
sources (`web/src/lib/branding.ts`, `web/src/EnactApp.tsx`, the logos, the
pairing CSS, the fonts) are fork-new and go in the overlay store.

**Work:**
- `project_enact_web_branding(source)` in `build.py`: one anchored
  `replace_required` per stock web file. Follow `project_enact_cli_command_strings`.
  - `web/src/main.tsx`: `./App` → `./EnactApp`
  - `web/src/components/layout/Sidebar.tsx`: logo → `brandLogo(resolvedTheme)`
  - `web/src/components/layout/Layout.tsx`: title → `brand.name`
  - `web/src/components/layout/ReloadBanner.tsx`: `zeroclaw reload` → `{brand.command} reload`
  - `web/src/components/sections/BindChannelForm.tsx`: `zeroclaw channel bind-telegram` → `{brand.command} ...`
  - `web/src/components/auth/SignInScreen.tsx`: `zeroclaw gateway get-paircode` → `{brand.command} ...`, logo
  - `web/src/components/SettingsModal.tsx`: `'ZeroClaw'` → `t('sidebar.brand')`
  - `web/src/contexts/ThemeContext.tsx`: `accent: 'cyan'` → `brand.accent`
  - `web/src/pages/AcpConsole.tsx`: `'ZeroClaw ACP'` → `` `${t('sidebar.brand')} ACP` ``
  - `web/src/lib/i18n.ts`: wrap the translation lookup in `brandDisplayText`
  - `web/src/contexts/turnStream.logic.test.ts`: blank line
  - `web/src/components/auth/SignInScreen.test.ts`: fork assertion
  - `web/index.html`: `%ENACT_NAME%` / `%ENACT_LOGO%` / `%ENACT_LOGO_DARK%` placeholders + `enact-pairing.css`
  - `web/package.json`: `test:branding` into `pretest`
  - `web/tsconfig.app.json`: `resolveJsonModule: true`
  - `web/vite.config.ts`: the `enact-brand-html` plugin
- Overlay store: `web/src/lib/branding.ts`, `web/src/lib/branding.test.ts`,
  `web/src/EnactApp.tsx`, `web/public/enact-logo*.svg`,
  `web/public/enact-pairing.css`, `web/public/fonts/*`.

**The build path.** `build.py` must stop copying the tracked `web/dist` and
instead build the web **in the prepared tree**:
```
npm --prefix <prepared>/web ci
cargo web gen-api --manifest-path <prepared>/Cargo.toml   # resolves repo_root() to the prepared tree
npm --prefix <prepared>/web run build
```
`cargo web` from the prepared tree needs no `xtask` change (compile-time
`CARGO_MANIFEST_DIR`).

**Risk:** `npm ci` per release. Acceptable; the release is not a hot path.

### B. `.github/workflows/` — 9 stock files → FORK-OWNED TWINS

**The blocker.** Upstream's `ci.yml` pins Blacksmith runners in 52 places.
This fork has no Blacksmith app, so upstream's Quality Gate would queue
forever and create no jobs. The fork replaced `ci.yml` wholesale (194
insertions, 366 deletions) to run on `ubuntu-24.04`.

**What fires on the fork once the stock workflows are restored** (measured —
every stock workflow's triggers):

| Stock workflow | Trigger | Fires on this fork? |
| --- | --- | --- |
| `ci.yml` | `pull_request`/`push` → `master` | **No** — the fork has no `master` |
| `windows-tests.yml` | `pull_request` → `master` | **No** |
| `docker-image-pr.yml` | `pull_request` → `master` | **No** |
| `platform-tests.yml` | `master` + **daily cron 03:17** | **Yes** — scheduled |
| `aur-freshness-check.yml` | **weekly cron Mon 07:41** | **Yes** |
| `cross-platform-clippy.yml` | **weekly cron Mon 10:17** | **Yes** |
| `daily-audit.yml` | **daily cron 09:00** | **Yes** |
| `daily-npm-audit.yml` | **daily cron 09:23** | **Yes** |
| `scoop-bucket-canary.yml` | **weekly cron Mon 07:23** | **Yes** |

So restoring the stock workflows leaves **7 of the 9 running on a schedule**.
Two of them (`ci.yml`, `windows-tests.yml`) use Blacksmith labels and would
queue forever; the scheduled ones use `ubuntu-24.04` and would run upstream's
version against the fork — which is *wrong behaviour for the fork* but not a
hang.

**Work:**
- The fork's Quality Gate moves to a **fork-new** file, e.g.
  `.github/workflows/enact-agent-quality-gate.yml`, with the fork's content.
  The stock `ci.yml` is restored to upstream's bytes.
- `codeql.yml`, `docker-image-pr.yml`, `release-stable-manual.yml`,
  `windows-tests.yml` → fork-new twins; the stock files restored.
- The 7 scheduled stock workflows → fork-new twins that carry the fork's
  content and disable the stock schedule **from the fork side** is not
  possible (the stock file's trigger is stock text). Options:
  - (a) Leave the stock scheduled workflows running upstream's logic against
    the fork. They would use upstream's scripts, which the fork no longer
    carries → they would fail.
  - (b) Fork-new twins with the same cron, and accept that the stock ones
    also fire and fail. **Bad — red runs.**
  - (c) **Disable the stock workflows at the repository-settings level**
    (GitHub Actions → disable the workflow). The file stays byte-identical to
    upstream in the tree; only the repo's settings change. **This is the
    correct answer** and it is what "zero touching of stock files" implies:
    the *file* is untouched; the fork's *repo configuration* chooses not to
    run it.

**Recommendation: (c).** The stock files stay byte-identical to upstream and
are disabled in the fork's repo settings; the fork's own workflow files carry
the fork's behaviour. That satisfies "zero touching of stock files" exactly —
the rule is about file content, not about whether the fork invokes upstream's
workflows.

**This is the highest-risk item.** It changes what CI runs, and
`tests/architecture/ci_runner_labels.rs` asserts on the fork's workflow
content.

### C. `scripts/ci/` — 5 stock files → FORK-OWNED TWINS

| Stock file | Fork change | Twin |
| --- | --- | --- |
| `apt_install.sh` | 300s deadline, 3 attempts | `scripts/ci/enact/apt_install.sh` |
| `apt_install.test.sh` | asserts the new deadline | `scripts/ci/enact/apt_install.test.sh` |
| `cli_only_ratchet.sh` | accept the caller's feature list | `scripts/ci/enact/cli_only_ratchet.sh` |
| `install_cli_package_version_test.sh` | the fork's release resolution | `scripts/ci/enact/install_cli_package_version_test.sh` |
| `parallel_runtime_test_gate.sh` | per-crate failure budget | `scripts/ci/enact/parallel_runtime_test_gate.sh` |

The fork's workflows call the twins. The stock scripts are restored.

**Alternative:** the fork's workflow copies the stock script to a scratch
path and patches it there (the prepare-then-run pattern the overlay already
uses). That keeps the stock script's *name* usable by stock workflows.

### D. `install.sh` — OVERLAY STORE + the gate moves

- The **prepared** tree gets the fork's `install.sh` (overlay store), so the
  release ships it.
- The **tracked** `install.sh` stays stock → `cargo generate installers
  --check` passes against upstream's installer.
- `tests/architecture/enact_release.rs` (stock) reads the tracked
  `install.sh` and fails on upstream's. → the fork's gate becomes an
  overlay-store file, applied to the prepared tree, where it reads the
  prepared tree's fork `install.sh`.
- `tests/test_architecture.rs` (stock) registers the gate modules → the
  fork's registration also moves to the overlay store.

**Consequence.** The tracked tree loses the fork's release gate. The fork's
CI must run `cargo test --test architecture` **in the prepared tree**.

### E. `tests/architecture/ci_runner_labels.rs` — OVERLAY STORE

Same as D: the fork's gate reads the fork's own workflows. The fork's
version moves to the overlay store; the tracked one is stock.

### F. `docs/book/` — 2 stock files → PROJECTION

- `docs/book/src/SUMMARY.md`: add the Enact runbook line (projection).
- `docs/book/src/maintainers/ci-and-actions.md`: the fork's CI prose.
- The fork's `enact-release-runbook.md` is fork-new and stays tracked.

**Consequence.** The tracked book (upstream's) will not link the fork's
runbook. The fork's docs job builds from the prepared tree.

### G. `deny.toml` — FORK-OWNED CONFIG

Stock `deny.toml` fails on 9 advisories, all from upstream's own pins.
`cargo deny --config` exists.

**Work:** `overlays/enact/deny.toml` = the stock file plus the 9 advisory
waivers and the corrected `lru` wrapper bound. CI runs
`cargo deny --config overlays/enact/deny.toml check`. The tracked
`deny.toml` is restored to stock.

**Verified:** `advisories ok, bans ok, licenses ok, sources ok`.

**Note:** these waivers are upstream's problem, not the fork's. They should be
reported upstream and removed once upstream bumps `zip` and `lettre`.

### H. `nix/web.nix` — PROJECTION

The fork copies `overlays/enact/brand.json` into the web-only source tree.
Projection into the prepared tree; the tracked file stays stock. The fork's
Nix web build must run against the prepared tree.

### I. `AGENTS.md` — RESTORE TO STOCK

The fork's "Enact Fork Overlay" pointer moves out of the root `AGENTS.md`.
`overlays/enact/AGENTS.md` (fork-owned) already holds the contract, and the
fork's `overlays/enact/README.md` points at it. The root `AGENTS.md` is
restored to upstream's bytes.

## The gate becomes absolute

`overlays/enact/tracked-delta.json` is **deleted**. The waiver list goes
away entirely, because there is nothing left to waive.

```python
def require_zero_drift(base):
    drifted = upstream_drift(base)
    if drifted:
        raise RuntimeError(
            "Upstream-owned files carry fork text in the tracked tree:\n" + ...
        )
```

Every upstream-owned file must equal the pinned base. The release gate and a
CI job both run it.

## Work breakdown

| # | Item | Stock files touched | New/changed fork files | Risk |
| --- | --- | --- | --- | --- |
| 1 | Web branding projection | 16 web | `build.py`, 6 overlay files | medium |
| 2 | Build the web in the prepared tree | 0 | `build.py` | medium |
| 3 | Fork-owned workflow twins | 9 workflows | 9 new workflow files | **high** |
| 4 | Fork-owned CI script twins | 5 scripts | 5 new script files | medium |
| 5 | Fork's architecture gates → overlay store | 3 test files | 3 overlay files | medium |
| 6 | `install.sh` → overlay store | 1 | 1 overlay file | medium |
| 7 | Fork-owned `deny.toml` config | 1 | 1 new file | low |
| 8 | `docs/book` projection | 2 | `build.py` | low |
| 9 | `nix/web.nix` projection | 1 | `build.py` | low |
| 10 | Root `AGENTS.md` → stock | 1 | — | low |
| 11 | Absolute gate, delete `tracked-delta.json` | 0 | `build.py`, `test_build.py` | low |

## Verification

Per item:
- `git diff --exit-code "$base" -- . ':(exclude)overlays/'` → **empty** (the
  acceptance criterion, checked after every step).
- `python3 -B -m unittest discover -s overlays/enact -p test_build.py`
- `python3 overlays/enact/build.py prepare` → the prepared tree carries the
  fork's web, installer, gates, deny config.
- `cargo generate installers --check` → passes against the **stock**
  `install.sh`.
- `cargo deny --config overlays/enact/deny.toml check` → ok.
- `cargo clippy --locked --workspace --all-targets` in the prepared tree.
- `cargo test --locked --test architecture` in the prepared tree (the fork's
  gates) and in the tracked tree (the stock gates).
- `npm --prefix <prepared>/web run build` → the branded bundle builds.

## Open questions for you

**Q1. CI workflow twins.** Upstream's `ci.yml` pins Blacksmith runners and
would queue forever on this fork. **Resolved by measurement:**
- `ci.yml`, `windows-tests.yml`, `docker-image-pr.yml` trigger only on
  `master` → dormant on the fork.
- The other 6 of the 9 have `schedule:` triggers → they **do** fire.
- All 33 workflows are currently `active` on `adl-tan-theta/enact-agent`
  (verified with `gh workflow list`).
- `gh workflow disable <name>` exists (verified). Disabling a workflow
  changes **only the repo's settings**, not the file.

**Recommendation:** restore every stock workflow file to upstream's bytes and
`gh workflow disable` the ones the fork replaces, in the fork's repo
settings. The fork's own workflow files carry the fork's behaviour. The rule
is about file content, and the files stay byte-identical.

*Confirm this is the intent: the fork stops running upstream's scheduled
workflows and runs its own instead.*

**Q2. `cargo deny`.** The 9 waivers are upstream's problem — upstream's own
`zip 9.0.3` and `lettre 0.11.22` pins, plus `chrono`/`chrono-tz`.
**Verified:** `cargo deny --config overlays/enact/deny.toml check` →
`advisories ok, bans ok, licenses ok, sources ok` with the stock `deny.toml`
untouched. Accept the fork-owned config, or report the 9 upstream advisories
upstream first and wait for upstream to bump `zip`/`lettre`?

**Q3. The web build.** Building the web in the prepared tree means `npm ci`
runs per release. Acceptable? (The release is not a hot path, and the web
build is already a manual step today.)

**Q4. The tracked tree loses the fork's release gate** (`enact_release.rs`)
and the fork's runner-label gate, because both are stock files that must
stay stock. The fork's CI must run `cargo test --test architecture` **in the
prepared tree** instead. The fork's native-build workflow already builds
from the prepared tree, so the pattern exists. Confirm.

## Sequencing

The items are independent except for the last. Do them in this order, so
each step is verifiable on its own:

1. **Q2** — the `deny.toml` fork-owned config. Smallest, fully verified
   already. Restores `deny.toml` to stock.
2. **Q10** — the root `AGENTS.md` to stock. Trivial.
3. **Q8/Q11** — the `docs/book` and `nix/web.nix` projections.
4. **Q6/Q5** — the `install.sh` overlay store and the fork's architecture
   gates into the overlay store. These belong together: the fork's gate
   reads the prepared tree's fork `install.sh`.
5. **Q1** — the web branding projection, the overlay store's fork web
   sources, and building the web in the prepared tree. The largest single
   change to the build.
6. **Q3/Q4** — the fork-owned CI script twins and the workflow twins. The
   highest risk; do it once 1–5 are in and the prepared tree is proven.
7. **Q7/Q9** — the absolute gate and deleting `tracked-delta.json`. Last,
   because until then the gate is the safety net.

**After every step:** `git diff --exit-code "$base" -- . ':(exclude)overlays/'`
must stay empty for the files that step handled.
