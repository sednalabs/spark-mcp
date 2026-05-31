# spark-mcp provenance + startup admission design

## Goal
Define a deterministic runtime identity contract and startup admission policy so operators can
verify what binary is running and whether required test-gate evidence is present.

## Provenance contract
- Build identity: `component@server_version+git_sha[-dirty]`.
- Source fingerprint: `git:<revision>:clean|dirty`.
- Runtime fields:
  - `pid`
  - `executable_path`
  - `binary_size_bytes`
  - `binary_modified_unix_ms`
- Attestation envelope shape: v2 (`schema_version=2`) with
  `extensions.runtime_admission`.

## Startup admission contract
- Inputs:
  - runtime provenance (`build_identity`, `source_fingerprint`, `component`)
  - gate profile (`fast` or `standard`)
  - gate artifact path (`data/test-gates/spark-mcp/*.json` by default)
- Required gate checks:
  - artifact exists and is readable
  - JSON schema version is supported
  - `component`, `gate_level`, `status`, `build_identity`, `source_fingerprint` match runtime
  - `command_manifest_digest` uses `sha256:` prefix
  - `expires_at` is valid RFC3339 and not expired
  - artifact is not older than executable
- Outcomes:
  - `passed`
  - `warn` (degraded start)
  - `rejected` (fail-closed)
  - `bypassed` (explicit break-glass)
  - `disabled`

## Mode semantics
- Default non-production: `warn` + `fast` profile.
- Production: defaults to `strict` + `standard` profile.
- Production may not run with mode `off`.
- Bypass requires explicit reason + TTL; production bypass requires explicit opt-in.

## Surface plan
- `/health`: include `indexed_at_unix_ms`, `provenance`, and `runtime_admission`.
- `/attest`: return the v2 attestation envelope.

## Verification plan
- Unit tests for admission evaluator (missing, stale, expired, valid).
- Unit tests for provenance derivation (`build_identity`, `source_fingerprint`).
- `cargo test` + release smoke run with semantic disabled.
