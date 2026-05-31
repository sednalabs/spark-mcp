# SPARK corpus

Place source documents for indexing in this directory. Suggested inputs include:

- SPARK language/spec docs
- Ada language references
- Additional local documents (HTML, PDFs, ZIPs)

Recommended structure:

- `spark-reference-manual/` — SPARK Reference Manual HTML bundle(s)
- `spark-user-guide/` — SPARK User's Guide (GNATprove + flow analysis) HTML bundle(s)
- `spark-reference-manual-live-<wave>/` — optional live/wave SPARK RM snapshot(s)
- `spark-user-guide-live-<wave>/` — optional live/wave SPARK UG snapshot(s)
- `spark-gnatprove-diagnostics/` — curated GNATprove explain-code markdown slice
- `why3-reference/` — optional Why3 manual HTML slice for prover-bridge queries
- `ada-reference-manual/` — Ada Reference Manual text + HTML bundle(s)
- `ada-2022-overview/` — Ada 2022 overview HTML
- `ada-2022-aarm/` — Ada 2022 Annotated Reference Manual (AARM) HTML
- `learning-ada_code/` — extracted learning-ada code assets
- `spark2014/` — AdaCore SPARK 2014 repo (docs + examples)
- `spark/` — SPARK specs, user guides, GNATprove docs (optional)
- `ada/` — Ada reference manuals, tutorials (optional)

Notes:
- Store extracted folders rather than ZIP archives in this corpus.
- If you add large corpora (e.g., a full Git repo), consider placing it under a
  named subfolder (e.g., `spark2014/`) so indexing rules can target it explicitly.
- Add a `.spark-mcp-ignore` file (gitignore syntax) if you need to exclude
  noisy or low-signal subtrees from indexing.
- See `SOURCES.md` for provenance and `scripts/ingest_docs.sh` for refresh steps.
- Optional live/wave ingestion:
  `SPARK_MCP_INCLUDE_LIVE_WAVE_DOCS=1 SPARK_MCP_LIVE_WAVE_IDS=spark2014 ./scripts/ingest_docs.sh`
- Optional diagnostics toggle (enabled by default):
  `SPARK_MCP_INCLUDE_GNATPROVE_DIAGNOSTICS=0 ./scripts/ingest_docs.sh`
- Optional Why3 bridge ingestion (disabled by default):
  `SPARK_MCP_INCLUDE_WHY3_BRIDGE=1 ./scripts/ingest_docs.sh`
