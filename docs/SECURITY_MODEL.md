# Security Model

`spark-mcp` exposes read-only search and retrieval over an operator-managed
SPARK/Ada corpus, plus one explicit audited maintenance operation:
`spark.reindex`.

## Authentication

All MCP calls to `/mcp` require `Authorization: Bearer <token>`.

Supported auth modes:

- `jwks`: validates JWTs with an authorization server JWKS endpoint.
- `introspection`: validates tokens through an OAuth introspection endpoint.
- `delegation`: local shared-secret mode for development and controlled smoke
  tests.

The default required scope is `spark:read`. Override it with
`SPARK_MCP_AUTH_REQUIRED_SCOPES`.

Optional hardening:

- `SPARK_MCP_AUTH_ALLOWED_CLIENT_IDS`: restrict accepted `azp` or `client_id`
  values.
- `SPARK_MCP_AUTH_SCOPES_SUPPORTED`: advertise an explicit supported-scope
  list in discovery metadata.
- `SPARK_MCP_AUTH_INTROSPECTION_*`: enable revocation-aware token checks.
- `SPARK_MCP_AUTH_JTI_ENFORCE_BEARER`: require `jti` claims for bearer replay
  protection when appropriate for the token profile.

## Public endpoints

These endpoints are public by design:

- `/health`
- `/attest`
- `/.well-known/oauth-protected-resource`
- `/.well-known/oauth-authorization-server`
- `/.well-known/openid-configuration`

`/health` includes index freshness, session metadata, runtime provenance, and
startup admission state. `/attest` returns a v2 attestation envelope with
runtime identity and binary metadata.

## Corpus boundaries

The baseline corpus is stored under `corpus/` and documented in
`corpus/SOURCES.md`. Optional workspace mounts are indexed from
`SPARK_MCP_WORKSPACE_ROOT` without copying content into `corpus/`.

The indexer applies:

- per-file size limits;
- extension allowlists;
- ignored generated/cache/build directories;
- stable document identifiers derived from source labels;
- path validation for document, chunk, hover, and source-filter calls.

Do not commit generated index data, semantic artifacts, replay stores, or
download caches.

## Reindex posture

Spark's current index build path is whole-index oriented, so `spark.reindex`
validates a local scope request but refreshes the lexical index as one
single-flight operation. The default tool path is local-only, requires an audit
reason, rejects traversal-style workspace paths, and reserves broad source
selection for `full_reindex=true` with `SPARK_MCP_REINDEX_ALLOW_FULL=1`.

`spark.index_status` reports the current freshness state and the exact
`spark.reindex` command to run when local sources are stale. See
`docs/spark-reindex-parity-no-go-2026-02-23.md` for the historical no-go that
this contract supersedes.

## Hover controls

`spark.hover` is lexical/processless. It resolves file inputs against indexed
corpus or workspace sources and returns symbol/snippet context rather than
calling an external compiler or prover process.

## Semantic search

Semantic search is disabled by default. When enabled, embeddings are built from
the configured corpus/workspace sources and stored under
`SPARK_MCP_SEMANTIC_INDEX_DIR` (`data/semantic` by default). For predictable
startup, build the semantic index out of process with `spark-embed`, then start
the server with `SPARK_MCP_SEMANTIC_BUILD_ON_START=0`.

## Startup admission

Startup admission compares runtime provenance with a configured test-gate
artifact. Modes:

- `warn`: start and report degraded admission details.
- `strict`: fail closed when required evidence is missing, stale, expired, or
  mismatched.
- `off`: disable admission outside production mode.

Production mode uses strict defaults and rejects `off`. Break-glass bypasses
must include a reason and TTL; production bypass additionally requires explicit
opt-in.
