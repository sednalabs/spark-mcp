# Getting Started

This guide fetches a baseline SPARK/Ada corpus, starts the server with local
delegation auth, and shows how to opt into semantic search.

## Prerequisites

- Rust toolchain compatible with the repository.
- The command-line tools used by `scripts/ingest_docs.sh`.
- A bearer token accepted by the configured auth mode for MCP calls.

Generated indexes, semantic artifacts, replay stores, and caches are written
under `data/`, `.tmp/`, or `.fastembed_cache/` and should not be committed.

## Fetch the baseline corpus

```bash
./scripts/ingest_docs.sh
```

The script fetches the baseline sources described in `corpus/SOURCES.md`.
Useful toggles:

| Variable | Purpose |
| --- | --- |
| `SPARK_MCP_INCLUDE_LIVE_WAVE_DOCS=1` | Include version-aware live/wave documentation snapshots. |
| `SPARK_MCP_LIVE_WAVE_IDS=spark2014` | Select comma-separated live/wave IDs. |
| `SPARK_MCP_INCLUDE_GNATPROVE_DIAGNOSTICS=0` | Skip the curated GNATprove explain-code slice. |
| `SPARK_MCP_INCLUDE_WHY3_BRIDGE=1` | Include the optional Why3 bridge slice. |

## Start with local delegation auth

```bash
export SPARK_MCP_BIND_ADDR=127.0.0.1:9410
export SPARK_MCP_AUTH_MODE=delegation
export SPARK_MCP_AUTH_DELEGATION_SECRET=dev-secret
export SPARK_MCP_AUTH_DELEGATION_ISSUER=spark-mcp
export SPARK_MCP_AUTH_DELEGATION_AUDIENCE=spark-mcp
export SPARK_MCP_AUTH_REQUIRED_SCOPES=spark:read

SPARK_MCP_REINDEX=1 SPARK_MCP_SEMANTIC_ENABLED=0 \
cargo run --release --bin spark-mcp
```

## Start with JWKS auth

```bash
export SPARK_MCP_AUTH_MODE=jwks
export SPARK_MCP_AUTH_ISSUER="https://auth.example.com/realms/example"
export SPARK_MCP_AUTH_JWKS_URL="https://auth.example.com/realms/example/protocol/openid-connect/certs"
export SPARK_MCP_AUTH_AUDIENCE="spark-mcp"
export SPARK_MCP_AUTH_REQUIRED_SCOPES=spark:read

SPARK_MCP_REINDEX=1 SPARK_MCP_SEMANTIC_ENABLED=0 \
cargo run --release --bin spark-mcp
```

## Optional workspace mounts

Local workspace mounts are not copied into `corpus/`; they are indexed from the
configured workspace root.

```bash
export SPARK_MCP_WORKSPACE_ROOT=/path/to/workspace
export SPARK_MCP_INCLUDE_WORKSPACE=1
export SPARK_MCP_INCLUDE_WORKSPACE_FSTAR=1
export SPARK_MCP_INCLUDE_WORKSPACE_RUST=1
```

Mount labels include `local-spark`, `local-fstar`, and `local-rust`.

## Optional semantic search

Build embeddings out of process for reliable startup:

```bash
SPARK_MCP_SEMANTIC_ENABLED=1 SPARK_MCP_REINDEX=1 \
cargo run --release --bin spark-embed
```

Then start the server without building embeddings on startup:

```bash
SPARK_MCP_SEMANTIC_ENABLED=1 SPARK_MCP_SEMANTIC_BUILD_ON_START=0 \
cargo run --release --bin spark-mcp
```

Supported modes for `spark.search` are `auto`, `lexical`, `semantic`, and
`hybrid`. `auto` falls back to lexical when semantic search is disabled.

## Smoke checks

Health and attestation are intentionally public:

```bash
curl -fsS http://127.0.0.1:9410/health
curl -fsS http://127.0.0.1:9410/attest
```

MCP calls to `/mcp` require `Authorization: Bearer <token>`.

Repository smoke scripts:

```bash
./scripts/replay_smoke_test.sh
./scripts/prompt_probe.sh
```
