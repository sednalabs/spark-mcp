# spark-mcp

`spark-mcp` is a Streamable HTTP MCP server for grounded search and retrieval
over a local SPARK/Ada corpus. It indexes operator-managed documentation and
source material, then exposes citation-first MCP tools for agent workflows.
Retrieval tools are read-only; `spark.reindex` is an explicit audited
maintenance tool.

The default path is deterministic lexical search. Semantic search is optional
and uses a local embedding index when explicitly enabled.

## At a glance

- Transport: Streamable HTTP MCP on `/mcp`.
- Default bind: `127.0.0.1:9410`.
- Search modes: lexical by default; semantic and hybrid when enabled.
- Auth: mandatory JWT/JWKS, introspection, or local delegation-token mode.
- Public operational endpoints: `/health`, `/attest`, and OAuth discovery.
- Corpus provenance: `corpus/SOURCES.md`.
- License: Apache-2.0.

## Tool surface

Default tools:

- `spark.search`
- `spark.list_sources`
- `spark.get_doc`
- `spark.get_chunk`
- `spark.index_status`
- `spark.reindex`
- `spark.hover`
- `spark.llm_answer`
- `spark_locate`
- `spark_refs`

`spark.llm_answer` is a provider-agnostic adapter surface. It remains
unconfigured until a provider runtime is enabled by the operator.

## Documentation

- [Getting started](docs/GETTING_STARTED.md): ingest, local run commands,
  semantic-search setup, and smoke checks.
- [Security model](docs/SECURITY_MODEL.md): auth posture, corpus boundaries,
  public endpoints, and startup admission.
- [Tool guide](docs/TOOL_GUIDE.md): tools, prompts, resources, search modes,
  hover, and snapshot contracts.
- [Corpus sources](corpus/SOURCES.md): upstream source, license, local path, and
  refresh metadata for indexed third-party material.
- [Reindex parity decision](docs/spark-reindex-parity-no-go-2026-02-23.md):
  historical no-go, superseded by the audited `spark.reindex` contract.
- [Dependency governance](docs/dependency-governance.md): dependency selection
  and upgrade policy.

## Minimal local run

```bash
./scripts/ingest_docs.sh

export SPARK_MCP_AUTH_MODE=delegation
export SPARK_MCP_AUTH_DELEGATION_SECRET=dev-secret
export SPARK_MCP_AUTH_DELEGATION_ISSUER=spark-mcp
export SPARK_MCP_AUTH_DELEGATION_AUDIENCE=spark-mcp
export SPARK_MCP_AUTH_REQUIRED_SCOPES=spark:read

SPARK_MCP_REINDEX=1 SPARK_MCP_SEMANTIC_ENABLED=0 \
cargo run --release --bin spark-mcp
```

Delegation mode is intended for local development and smoke testing. Production
deployments should use JWKS or introspection with an external authorization
server and should keep the default loopback binding unless a trusted reverse
proxy is enforcing network policy.

Codex device login is provided through the shared `mcp-toolkit-rs` OAuth
metadata surface. After a JWKS/introspection deployment exposes
`/.well-known/oauth-authorization-server/mcp`, authenticate the local Codex MCP
entry with:

```bash
codex mcp login spark_mcp --device-auth
```

## Operational contract

`/health` is public and returns index freshness, session stats, runtime
provenance, and startup admission state. `/attest` is public and returns a v2
attestation envelope with runtime identity and binary metadata.

Tool schemas are snapshot-tested at
`spec/tool_schema_snapshot.v1.json`.

GitHub-hosted Rust Validation publishes a short-lived
`spark-mcp-linux-x86_64-<run_id>` artifact containing a release binary and
manifest. Local services should consume that hosted artifact rather than
building on operator machines.
