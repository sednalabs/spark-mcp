#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROBE_DIR="$ROOT_DIR/../../tools/mcp-probe"
PROBE_DIST="$PROBE_DIR/dist/index.js"
MCP_URL="${SPARK_MCP_URL:-http://127.0.0.1:9410/mcp}"
OUT_PATH="${SPARK_MCP_PROMPT_PROBE_OUT:-$ROOT_DIR/.tmp/prompt_probe.json}"

if [[ ! -f "$PROBE_DIST" ]]; then
  echo "mcp-probe not built. Run:" >&2
  echo "  npm -C $PROBE_DIR install" >&2
  echo "  npm -C $PROBE_DIR run build" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUT_PATH")"

node "$PROBE_DIST" run \
  --transport streamable-http \
  --url "$MCP_URL" \
  --pretty \
  --include-raw \
  --out "$OUT_PATH"

echo "Prompt probe report written to $OUT_PATH"
