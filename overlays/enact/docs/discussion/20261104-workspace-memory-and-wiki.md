# Workspace memory and wiki: data model and reference starting points

Status: discussion notes and a design proposal, not a decision or a contract.
Recorded 2026-11-04. Companion to
[saas-agent-platform.md](saas-agent-platform.md) and
[event-driven-agents.md](event-driven-agents.md). Upstream base examined:
`1c1cd118` (ZeroClaw 0.8.7).

This file does not relax `overlays/enact/AGENTS.md`. Nothing here has been built,
run or benchmarked. Claims are tagged:

- **[verified]** read in this repository, or on the named project's own page.
- **[unverified]** inference, vendor claim, or not checked.

Project facts below come from README and repository pages only. No candidate's
code was run, and no benchmark was reproduced.

## 1. Requirement

Memory for a workspace: emails, calendars, discussions, and artifacts that users
create (pages, slides). It needs episodic recall, durable facts, and a wiki whose
pages are linked to the things they came from. It should run locally and as a
server. Agents run in durable sandboxes with the workspace as the isolation
boundary.

## 2. What ZeroClaw already has

- **[verified]** `crates/zeroclaw-memory` has SQLite, Postgres (`memory-postgres`),
  Qdrant (`qdrant.rs`, REST), Lucid and Markdown backends, a knowledge graph
  module, embeddings, reranking, consolidation, decay and dedup.
- **[verified]** `MemoryCategory` is `Core`, `Daily`, `Conversation` or
  `Custom(String)`. There is no first-class episode with source, time,
  participants and links, and no provenance edges.
- **[verified]** The crate contains `principal_plane.rs`, `sqlite_permissions.rs`
  and `policy_gate.rs`. They were not read, so how permissions work is
  **[unverified]**.
- **[verified]** `overlays/enact/AGENTS.md` requires tracked `src/`, `crates/` and
  `Cargo.*` to match the pinned upstream base. A new crate would break that, so a
  memory and wiki service should be a separate project reached over MCP, unless
  the contract is amended.

## 3. Licensing note

**[verified]** ZeroClaw is `MIT OR Apache-2.0` (`Cargo.toml`, `LICENSE-MIT`,
`LICENSE-APACHE`). Apache-2.0 dependencies such as LanceDB and Qdrant are
therefore compatible with how it ships. The practical filter is "permissive":
MIT, Apache-2.0, BSD. Exclude AGPL, BSL and SSPL. This is an engineering
position, not legal advice; keep `NOTICE` obligations in mind.

## 4. Proposed model: one ID space, three node kinds

| Kind | ID prefix | Meaning | Mutable |
| --- | --- | --- | --- |
| Episode | `ep_` | Raw event: email, calendar event, message, or saved page or slide, with source, time and participants | No; tombstoned on deletion |
| Fact | `fact_` | Triplet (subject, predicate, object) with validity interval, confidence and links to source episodes | No; superseded by newer facts |
| Page | `pg_` | Versioned wiki page that cites facts and episodes by ID | Yes, with version history |

Entities (people, customers, projects) use `ent_` IDs and appear as subjects and
objects of facts. An entity may have a page. References use IDs, never titles.

**Direction rule.** Episode, then fact, then page. Lower layers are the source of
truth. Higher layers are derived and rebuildable. A human correction to a page is
recorded as a new episode of kind `assertion`, and the fact and page layers update
from it, so every claim keeps provenance.

```text
node(id, kind, workspace_id, created_at, acl_scope, tombstoned_at)
episode(id, source, source_ref, occurred_at, participants[], body, embedding)
fact(id, subject, predicate, object, valid_from, valid_to, confidence, superseded_by)
page(id, entity_id?, current_version, body_ref)
edge(from_id, to_id, kind)  -- derived_from, cites, mentions, attached_to, supersedes
```

The `edge` table must be queryable in both directions; recall and deletion both
depend on it.

### Storage mapping (proposal, untested)

- Local: SQLite tables with FTS5, a vector index (LanceDB or sqlite-vec), pages as
  markdown files in git.
- Server: Postgres tables, Qdrant or pgvector vectors, page bodies in git or
  object storage with an index row.
- Triplets fit relational tables plus the edge table at this scale. Add a graph
  engine only if queries need deep traversal.

### Recall

1. Resolve entities in the query.
2. In parallel: vector and keyword search over episodes and pages, plus 1 to 2 hop
   expansion through facts and edges, restricted to facts valid at the requested
   time.
3. Fuse the rankings (reciprocal rank fusion).
4. Apply permissions inside the query, before anything is returned.
5. Return IDs with citations. Prefer pages for overviews; drill to episodes for
   exact details.

### Write path

1. Ingest creates an episode, idempotent on `source` plus `source_ref`.
2. An LLM proposes facts, each linked to its source episodes, with confidence.
3. Entity resolution merges duplicates. This is the hard part.
4. Affected pages are queued for update; pinned pages need human approval.

### Rules that decide whether it holds up

- **Permissions.** A derived node inherits the strictest access of its sources.
  If that cannot be computed, mark it restricted. An email visible to one user must
  never surface for another user in the same workspace.
- **Deletion.** Delete an episode, then follow provenance edges: facts whose only
  source was it are deleted; facts with other sources lose that link; pages that
  cited it are marked stale and regenerated or redacted; embeddings are removed; a
  content-free tombstone remains for audit.
- **Noise.** LLM-extracted triplets are noisy. Store confidence, never treat as
  ground truth. Begin with episodes and pages, and add facts for entity types that
  matter: customers, owners, deadlines, decisions.
- **Contradictions** are normal. Supersede with validity intervals; keep history.

### Agent interface

Expose it as a tool group on the shared MCP server: `remember`, `recall` (with
filters and `as_of`), `get`, `facts.query`, `wiki.get`, `wiki.propose_edit`,
`link`, `forget`. Identity comes from the sandbox credential, never from a tool
argument.

## 5. Reference starting points

Nothing below is shown to be "super fast" for this workload. The only numbers are
the projects' own, on one machine each. Benchmark against your own data and
queries before choosing.

### Closest to a working Rust memory service

| Project | License | What it is (from its page) | Caveats |
| --- | --- | --- | --- |
| memrust 0.6.1, likely [AIAnytime/memrust](https://github.com/AIAnytime/memrust) | Apache-2.0 | Single-crate Rust memory engine. In-crate HNSW and BM25, entity graph with 1-hop traversal, reciprocal rank fusion, recency decay. `remember`, `recall`, `forget` over axum HTTP and MCP stdio. JSON-lines WAL with fsync, checkpoints, tombstone forgetting. API keys scoped to namespaces; each namespace is a separate engine; agent-owned memories are private by default and visibility is enforced in the pre-filter. | Repo URL inferred from notebook links. README calls itself "young", single node and single writer. Roadmap and body text disagree on whether auth is done. Ingest is "an order of magnitude behind" FAISS in its own comparison (988/s vs 35,625/s). Benchmarks are self-run on an M-series laptop with 20k vectors. Entity extraction is heuristic. |
| rust-memex 0.6.5, [Loctree/rust-memex](https://github.com/Loctree/rust-memex) | **Not stated** | LanceDB vectors plus tantivy BM25, MCP over stdio, and an HTTP/SSE daemon so several agents share one pool (works around LanceDB's exclusive lock). Bearer token, namespaces, optional dashboard OIDC. | Repo URL inferred. License not stated on the page: check before any use. Credits "vibecrafted with AI agents". Multi-tenancy is metadata filters only. Embedding dimension fixed per table. No latency numbers. Source builds discouraged because of LanceDB's dependency weight. |

### Building blocks

| Project | License | Use | Caveats |
| --- | --- | --- | --- |
| [tantivy](https://github.com/quickwit-oss/tantivy) | MIT | Embedded BM25 and faceted search; 16.4k stars; used in production by several companies | Immutable documents; edits need delete and reindex; commit and reload for visibility |
| [Qdrant](https://github.com/qdrant/qdrant) | Apache-2.0 | Server and cloud vector store with hybrid dense and sparse search; Qdrant Edge runs in-process; already supported by ZeroClaw | Edge maturity not checked |
| [LanceDB](https://github.com/lancedb/lancedb) | Apache-2.0 | Embedded vector store on disk or object storage; Rust crate | Hybrid search not confirmed on its page; Enterprise terms not checked |
| [Oxigraph](https://github.com/oxigraph/oxigraph) | MIT OR Apache-2.0 | Embedded Rust RDF triple store on RocksDB; SPARQL server mode | "SPARQL query evaluation has not been optimized yet"; "heavy development"; no bi-temporal support stated; RDF may be heavier than needed for facts |
| vecdb-core 0.1.0 (likely `zaydmulani09/vecdb`) | MIT | Embeddable vector DB with HNSW, BM25, hybrid fusion and an optional axum server | Version 0.1.0; HNSW build is slow (about 2.5 hours at 1M vectors on a laptop); benchmark file not seen |
| sqlite-vec | MIT OR Apache-2.0 | Vector search inside SQLite | Pre-v1; expect breaking changes; C core |

### Wiki layer

[geronimo-iia/llm-wiki](https://github.com/geronimo-iia/llm-wiki) is
`MIT OR Apache-2.0`: markdown pages with typed frontmatter, git history, tantivy
BM25, a petgraph link graph, multiple wiki spaces, MCP over stdio and HTTP, and
ACP over stdio. One author, 51 stars, no releases, no auth stated, no embeddings.

### Not Rust, but relevant

[Hindsight](https://github.com/vectorize-io/hindsight) is MIT, with Postgres and
pgvector, an MCP endpoint (retain, recall, reflect), and Python, TypeScript and Go
SDKs. It needs an LLM provider for retain and reflect, and its benchmark claims
are mostly vendor-reported.

## 6. Suggested path

1. Fix the data model in section 4 and write the permission and deletion rules as
   tests before choosing a stack.
2. Build a small evaluation set from real workspace data: a few hundred emails,
   events and notes with 30 to 50 questions, including permission-denied cases.
3. Read memrust for its WAL, namespace and pre-filter patterns. Prototype the
   recall path on tantivy plus one vector store. Compare against Hindsight and
   ZeroClaw's existing Qdrant memory on the evaluation set.
4. Decide between sidecars (Hindsight plus llm-wiki behind the MCP server) and an
   own Rust service, based on measured recall, latency and operating cost.

## 7. Open questions

1. Is the first goal recall quality, the wiki, or ingestion?
2. Data volume per workspace, and write rate?
3. Where do permissions come from for emails and calendars: the source system's
   ACLs, workspace membership, or both?
4. Retention and deletion requirements for email and calendar data?
5. One service per workspace sandbox, or a shared service with namespaces?
6. Do users edit wiki pages directly, or only through the agent?
7. Licensing of rust-memex must be confirmed before it is even used as a
   reference.
