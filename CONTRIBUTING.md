# Contributing

Thanks for improving `spark-mcp`.

## Development principles

- Keep default startup deterministic and lexical-first.
- Keep semantic search optional; it must not be required for server startup.
- Preserve mandatory bearer authentication for MCP calls.
- Treat corpus indexing as an input-validation surface. Preserve path
  traversal checks, file-size limits, source labels, stable document IDs, and
  whole-index refresh semantics.
- Do not add scoped in-process reindexing unless the no-go document's safety
  invariants are satisfied first.
- Avoid new dependencies unless the repository's dependency governance gates
  can be satisfied.

## Corpus provenance

If you add or refresh corpus sources, update `corpus/SOURCES.md` with:

- source URL;
- retrieval date;
- local path;
- license location or license status;
- refresh command or environment toggle.

Do not commit generated indexes, semantic artifacts, caches, or full upstream
clones that are intentionally ignored by repository policy.

## Documentation

Behavior changes should update the relevant public docs:

- `README.md`
- `docs/GETTING_STARTED.md`
- `docs/SECURITY_MODEL.md`
- `docs/TOOL_GUIDE.md`
- `corpus/SOURCES.md`

## Validation

For docs-only changes, run:

```bash
git diff --check
```

For behavior changes, run the repository's documented Rust tests and smoke
checks. If tool schemas change intentionally, update
`spec/tool_schema_snapshot.v1.json` through the documented snapshot-update
preset. If parity-sensitive tool presence changes, update the F*/Spark parity
docs and guardrail in `../fstar-mcp/`.

## Pull requests

Keep changes small and reviewable. Do not bundle generated indexes, local
tokens, private paths, environment-specific service files, or unreviewed corpus
downloads into a public PR.
