# Prompt probe (mcp-probe)

Use the repo's `mcp-probe` CLI to verify prompt discovery against the running
spark-mcp server without adding new dependencies to spark-mcp.

## Build the probe once
```bash
npm -C ../../tools/mcp-probe install
npm -C ../../tools/mcp-probe run build
```

## Run the prompt probe
```bash
./scripts/prompt_probe.sh
```

Override the MCP URL or output path if needed:
```bash
SPARK_MCP_URL=http://127.0.0.1:9410/mcp \
SPARK_MCP_PROMPT_PROBE_OUT=./.tmp/prompt_probe.json \
./scripts/prompt_probe.sh
```

## What to look for
The report includes the raw MCP `prompts/list` response. Confirm both prompt names:
- `spark.grounded_answer`
- `spark.grounded_answer_checklist`
