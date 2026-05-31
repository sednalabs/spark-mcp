# Corpus sources

This folder contains third-party documentation used for local search and LLM-assisted
retrieval. Keep this file updated when adding or refreshing sources.

## SPARK Reference Manual
- Source: https://docs.adacore.com/spark2014-docs/html/lrm/
- Format: HTML (mirrored)
- License: GNU Free Documentation License (see SPARK docs footer)
- Local path: corpus/spark-reference-manual/raw/
- Refresh: scripts/ingest_docs.sh
- Retrieved: 2026-02-24

## SPARK User's Guide (GNATprove)
- Source: https://docs.adacore.com/spark2014-docs/html/ug/
- Format: HTML (mirrored)
- License: GNU Free Documentation License (see SPARK docs footer)
- Local path: corpus/spark-user-guide/raw/
- Refresh: scripts/ingest_docs.sh
- Retrieved: 2026-02-24
- Notes: Primary GNATprove + flow-analysis reference; no PDF ingestion.

## SPARK Reference Manual (live/wave snapshots, optional)
- Source template: https://docs.adacore.com/live/wave/<wave>/html/<wave>_rm/
- Format: HTML (mirrored)
- License: GNU Free Documentation License (see SPARK docs footer)
- Local path template: corpus/spark-reference-manual-live-<wave>/raw/
- Refresh: scripts/ingest_docs.sh with `SPARK_MCP_INCLUDE_LIVE_WAVE_DOCS=1`
- Selection: `SPARK_MCP_LIVE_WAVE_IDS=<comma-separated-wave-ids>` (default `spark2014`)
- Retrieved: 2026-02-24
- Notes: Disabled by default. Each wave is stored under a distinct top-level folder to preserve stable source labels.

## SPARK User's Guide (live/wave snapshots, optional)
- Source template: https://docs.adacore.com/live/wave/<wave>/html/<wave>_ug/
- Format: HTML (mirrored)
- License: GNU Free Documentation License (see SPARK docs footer)
- Local path template: corpus/spark-user-guide-live-<wave>/raw/
- Refresh: scripts/ingest_docs.sh with `SPARK_MCP_INCLUDE_LIVE_WAVE_DOCS=1`
- Selection: `SPARK_MCP_LIVE_WAVE_IDS=<comma-separated-wave-ids>` (default `spark2014`)
- Retrieved: 2026-02-24
- Notes: Disabled by default. Use this for version-aware corpus slices without changing baseline ingest behavior.

## Ada 2022 Reference Manual
- Source: https://www.adaic.org/resources/add_content/standards/22rm/RM-22-Txt.zip
- Source: https://www.adaic.org/resources/add_content/standards/22rm/RM-22-Html.zip
- Format: TXT + HTML (zip)
- License: Ada 2022 RM license statement on adaic.org
- Local path: corpus/ada-reference-manual/text/
- Local path: corpus/ada-reference-manual/html/
- Refresh: scripts/ingest_docs.sh
- Retrieved: 2026-02-24

## Ada 2022 Annotated Reference Manual (AARM)
- Source: https://www.adaic.org/resources/add_content/standards/22aarm/AA-22-Html.zip
- Format: HTML (zip)
- License: Ada 2022 AARM license statement on adaic.org
- Local path: corpus/ada-2022-aarm/html/
- Refresh: scripts/ingest_docs.sh
- Retrieved: 2026-02-24

## Ada 2022 Overview
- Source: https://www.adaic.org/resources/add_content/standards/ada2022.html
- Format: HTML
- License: AdaIC content policy (see page footer)
- Local path: corpus/ada-2022-overview/
- Refresh: scripts/ingest_docs.sh
- Retrieved: 2026-02-24

## SPARK 2014 Git repository
- Source: https://github.com/AdaCore/spark2014
- Format: Git (shallow clone)
- License: see repository LICENSE
- Local path: corpus/spark2014/
- Retrieved: 2026-01-26

## GNATprove diagnostics explain codes (curated subset)
- Source: https://github.com/AdaCore/spark2014/tree/master/share/spark/explain_codes
- Format: Markdown subset copied from local `spark2014` corpus mirror
- License: inherits `spark2014` repository license
- Local path: corpus/spark-gnatprove-diagnostics/raw/
- Refresh: scripts/ingest_docs.sh (`SPARK_MCP_INCLUDE_GNATPROVE_DIAGNOSTICS=1`, default enabled)
- Retrieved: 2026-02-24
- Notes: Curated troubleshooting slice (`E0001`..`E0020` + README) to expose diagnostics as a dedicated high-signal source label.

## Why3 reference manual (optional bridge slice)
- Source: https://www.why3.org/doc/
- Format: HTML (mirrored)
- License: Why3 package license `LGPL-2.1-only` (opam metadata); docs include copyright notice
- License reference: https://opam.ocaml.org/packages/why3/why3.1.8.2/opam
- Local path: corpus/why3-reference/raw/
- Refresh: scripts/ingest_docs.sh (`SPARK_MCP_INCLUDE_WHY3_BRIDGE=1`)
- Retrieved: 2026-02-24
- Notes: Disabled by default. Curated bridge for prover/backend troubleshooting queries that cross SPARK and Why3.

## Local workspace mounts
- Source: local workspace repos (not third-party)
- Format: repo files under `spark/`, `rust/`, and `fstar/` as configured
- Local path: via `SPARK_MCP_WORKSPACE_ROOT` with labels (`local-spark`, `local-rust`, `local-fstar`)
- Exclusions: `target/`, `data/`, `corpus/` (skipped during indexing)
- Notes: local mounts are enabled by default for `spark/` and `fstar/` when the workspace root is set;
  `local-rust` remains opt-in. Mounts are not stored under `corpus/`.
