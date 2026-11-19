# Overlay file store

Whole-file fork versions of upstream-owned files.

`build.py`'s `prepare()` copies this store over the prepared tree at
`target/enact/source/`. The tracked tree keeps upstream's bytes for every path
named here, so an upstream merge does not have to fight the fork's text.

Each entry is the complete fork version of the file, at the file's own
repo-relative path:

```
files/apps/zerocode/src/jsonrpc.rs   ->  apps/zerocode/src/jsonrpc.rs
```

## When a file belongs here

When the file's only consumer is the prepared tree, and the fork's version is
a different file rather than upstream's file with a replacement in it.

When the file's version is upstream's file plus a small, identifiable change,
prefer an anchored `replace_required` projection in `build.py`. A projection
keeps the fork's intent visible next to the upstream text it edits, and it
fails closed when upstream drifts.

## When a file cannot belong here

The store reaches the prepared tree only. It does not reach the tracked tree,
and the tracked tree is where these read from:

- GitHub Actions reads `.github/workflows/*.yml`.
- The CI jobs run `scripts/ci/*`.
- The web build and the web tests read `web/`.
- `tests/architecture/enact_release.rs` reads `install.sh`.
- `cargo deny` reads `deny.toml`.
- The docs gate reads `docs/book/`.

Those files keep their fork text in the tracked tree, and
[`../tracked-delta.json`](../tracked-delta.json) records each one with the
reason it cannot be overlaid.

## Rules

- The tracked copy of a stored path must equal the pinned upstream base, byte
  for byte. `test_build.py` enforces this.
- The store is applied before the projections, because a projection anchors on
  upstream's text and has to see that text.
- A stored path that the prepared tree does not have is created. A stored
  path that escapes the repository root is refused.
