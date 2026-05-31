# Spark Scoped Reindex Parity Feasibility

Last updated: February 23, 2026

## Decision
`NO-GO` for now: do not add `spark.reindex` scoped in-process parity in the current architecture.

## Why (Safety Gate Outcome)
The current Spark indexing runtime cannot satisfy fail-closed scoped-reindex invariants without a
broader architecture change.

Key blockers:
1. `build_index(...)` is destructive by design (`writer.delete_all_documents()`), so partial/source
   scoped rebuilds cannot preserve unaffected sources safely.
2. `SearchIndex` runtime state is immutable for index metadata/source snapshots (`sources`,
   `index_metadata`) and does not expose a reindex concurrency lock (`reindex_lock`) equivalent.
3. There is no existing request/response contract for scoped path validation + audited reason + busy
   signaling analogous to the F* implementation.

## Invariants Required Before Implementation
Scoped in-process reindex parity should only be reconsidered once Spark has all of:
1. Non-destructive scoped rebuild semantics (or robust selective segment replacement) that guarantee
   no accidental source loss.
2. Explicit concurrency controls (single-flight lock + busy signaling).
3. Strict scope/path validation (relative-only, no traversal, local-root constrained).
4. Auditable reindex contract (`reason` required, deterministic report payload).
5. Negative tests proving fail-closed behavior for invalid scope/path and concurrent requests.

## Revisit Trigger
Re-open implementation only when an approved architecture change introduces safe incremental/scoped
index mutation semantics for Spark.

## References
- `servers/spark-mcp/src/search.rs`
- `servers/spark-mcp/src/search/runtime.rs`
- `servers/spark-mcp/src/search/indexing.rs`
- `servers/fstar-mcp/src/search/runtime.rs` (reference model)
