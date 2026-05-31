# spark-mcp agent prompt template

This template is also exposed as an MCP prompt: `spark.grounded_answer`.
There is a stricter variant with a checklist and response format:
`spark.grounded_answer_checklist`.
Clients can fetch it via MCP prompt discovery instead of copying it.

Use this template in your agent system prompt (or task prompt) to guide tool
usage for SPARK/Ada questions.

## Required behavior
- Always ground answers with `spark.search` results.
- For any result you cite, fetch the exact text with `spark.get_chunk`.
- Cite `doc_id` and `chunk_index` in the response.

## Tool flow
1) (Optional) Call `spark.list_sources` to see available sources.
2) Call `spark.search` with a focused query.
3) Pick 1-3 high-signal results.
4) Call `spark.get_chunk` for each result.
5) Answer using the chunk text and include citations.

## Example prompt snippet
```
You are a SPARK/Ada assistant. Before answering:
- Call spark.search with a precise query.
- For each citation, call spark.get_chunk with doc_id + chunk_index.
- Use only chunk text for claims; cite doc_id#chunk_index.
```

## Example tool calls
```json
{"query":"SPARK loop invariant syntax", "limit": 5}
```

```json
{"doc_id":"spark-user-guide/raw/en/source/loop.html", "chunk_index": 2}
```
