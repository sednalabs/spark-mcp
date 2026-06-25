# Dependency Governance

This document defines dependency selection and upgrade policy for this repository's Rust components.

## Goal

Keep Rust components secure, maintainable, and release-friendly by preferring well-maintained crates with clear operational risk signals.

## Scope

- Direct dependencies declared in Cargo manifests for the governed Rust workspaces
- Tooling dependencies used in release checks
- New crates and major/minor dependency upgrades

## Go/No-Go Criteria

All new direct crates and major upgrades must meet every hard gate below.

1. `security`: No unresolved RustSec advisory for selected version.
2. `license`: License is permitted by `deny.toml`.
3. `source`: Registry source is trusted (`crates.io` only by default).
4. `maintenance`: Evidence of active maintenance (recent releases, active issue/PR activity, non-abandoned project).
5. `adoption/reputation`: Evidence the crate is broadly used or maintained by a trusted team/project.
6. `fit`: Clear justification that existing dependencies or stdlib cannot solve the need with lower risk.

If any hard gate fails, the change is `no-go` unless an explicit, time-bounded exception is approved and documented.

## Required Evidence for Dependency Changes

Every dependency change (new crate, removed crate, major/minor upgrade) must include a policy note in the associated PR description.

Use this template:

```text
Dependency change note
- crate: <name> <old -> new>
- change type: <new | upgrade | removal>
- purpose: <why needed>
- alternatives considered: <stdlib/existing crates/other crates>
- maintenance evidence: <release recency + repo activity>
- adoption/reputation evidence: <reverse-deps/downloads/known users or maintainer org>
- security status: <cargo deny + cargo audit result>
- license status: <permitted license(s)>
- startup impact: <expected effect on cold start/steady state>
- rollback plan: <how to revert safely>
- exception (if any): <risk accepted, owner, expiry date>
```

## Enforcement

Run:

```bash
./scripts/dependency_governance_check.sh
```

The script enforces:

1. advisory/license/source policy via `cargo-deny` (blocking)
2. RustSec check via `cargo-audit` (blocking)
3. stale-risk scan on direct dependencies via `cargo-outdated` (report-only by default)

When `STRICT_OUTDATED=0`, a resolver conflict in the stale-risk report is
logged without failing the workflow.

Phase-2 tightening option:

```bash
STRICT_OUTDATED=1 ./scripts/dependency_governance_check.sh
```

When `STRICT_OUTDATED=1`, outdated direct dependencies become a failing gate.

## Exceptions

Exceptions are allowed only when there is a clear delivery blocker and no safer near-term option.

Exception requirements:

1. Documented in PR with rationale, owner, and explicit expiry date.
2. Bounded duration (target <= 30 days).
3. Follow-up issue created before merge.

Current release exceptions:

| Advisory | Source | Rationale | Expiry |
| --- | --- | --- | --- |
| `RUSTSEC-2024-0436` | `paste` via `tokenizers` 0.22.2 and `fastembed` 5.17.2 | Reviewed 2026-06-25. Updating the embedding stack to `fastembed` 5.17.2 does not remove `paste`; semantic search remains opt-in. Revisit when the embedding stack removes this path. | 2026-08-31 |
| `RUSTSEC-2025-0141` | `bincode` 1.3.3 via direct semantic embedding serialization and `hnsw_rs` 0.3.4 | Reviewed 2026-06-25. `hnsw_rs` 0.3.4 is the latest upstream release and still depends on `bincode` 1.x. Revisit before publishing semantic index artifacts or changing the serialization/index backend. | 2026-08-31 |
