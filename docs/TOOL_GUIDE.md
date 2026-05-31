# Tool Guide

`spark-mcp` exposes citation-first tools over the indexed SPARK/Ada corpus. All
tool calls require bearer auth at the HTTP layer.

## Tools

| Tool | Purpose |
| --- | --- |
| `spark.search` | Search the corpus with lexical, semantic, or hybrid retrieval. |
| `spark.list_sources` | List indexed source labels and file counts. |
| `spark.get_doc` | Fetch a full corpus document by `doc_id`. |
| `spark.get_chunk` | Fetch a specific chunk by `doc_id` and `chunk_index`. |
| `spark.index_status` | Report index metadata, corpus counts, freshness, and refresh hints. |
| `spark.hover` | Resolve lexical symbol/snippet context for a file location. |
| `spark.llm_answer` | Provider-agnostic grounded-answer adapter; inactive until configured. |
| `spark_locate` | Locate symbol definitions or references. |
| `spark_refs` | Find symbol usage sites. |

## Prompts

MCP prompt discovery exposes:

- `spark.grounded_answer`
- `spark.grounded_answer_checklist`

The prompt flow is:

1. Search with `spark.search`.
2. Fetch cited text with `spark.get_chunk`.
3. Answer using only fetched evidence and cite `doc_id#chunk_index`.

## Resources and templates

Static resources:

- `spark-mcp://help`
- `spark-mcp://index-status`

Resource templates:

- `spark-mcp://doc/<doc_id>`
- `spark-mcp://chunk/<doc_id>/<chunk_index>`
- Workspace markdown/spec templates when `SPARK_MCP_WORKSPACE_ROOT` is set.

## Search modes

`spark.search` accepts `mode`:

- `auto`: hybrid when semantic search is enabled, otherwise lexical.
- `lexical`: BM25 keyword search.
- `semantic`: embedding similarity search; requires semantic search enabled.
- `hybrid`: combines lexical and semantic scores; requires semantic search
  enabled.

`query_kind` controls lexical parsing:

- `literal`: plain-text/code-fragment search.
- `tantivy`: Tantivy query parser syntax.

Shortcut prefixes:

- `def:Symbol`
- `ref:Symbol`

Source filtering accepts `source=<label>` or `sources=[...]`. `sources`
overrides `source` when both are present. The `local` alias matches all
`local-*` workspace mounts.

## Hover flow

`spark.hover` is lexical/processless. It accepts absolute paths under an indexed
root, repository-relative paths, or `source:path` document identifiers. Hover
results are operational context, not corpus citations.

## Refresh flow

Use `spark.index_status` first. If local sources are stale, follow the reported
restart-with-reindex runbook. Scoped in-process reindexing is intentionally not
exposed in the current architecture.

## Snapshot contract

Tool schemas are snapshotted in `spec/tool_schema_snapshot.v1.json`. Intentional
tool-shape changes should update this guide, then rebaseline the snapshot with
the repository's documented snapshot-update workflow.
