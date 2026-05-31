#!/usr/bin/env bash
set -euo pipefail

MCP_URL="${SPARK_MCP_URL:-http://127.0.0.1:9410/mcp}"
AUTH_HEADER="${MCP_AUTH_HEADER:-${SPARK_MCP_AUTH_HEADER:-}}"
EVENT_DB="${SPARK_MCP_HTTP_EVENT_STORE_PATH:-data/event-store.sqlite}"

auth_args=()
if [[ -n "$AUTH_HEADER" ]]; then
  auth_args=(-H "Authorization: $AUTH_HEADER")
fi

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

headers="$tmp_dir/headers.txt"
init_payload='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"replay-smoke","version":"0.1"}}}'

curl -sS -D "$headers" -o "$tmp_dir/init.json" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  "${auth_args[@]}" \
  -X POST "$MCP_URL" \
  -d "$init_payload"

session_id="$(
  awk 'tolower($1)=="mcp-session-id:" {print $2}' "$headers" \
    | tr -d '\r' \
    | tail -n1
)"
if [[ -z "$session_id" ]]; then
  echo "Missing Mcp-Session-Id header from initialize." >&2
  exit 1
fi

list_payload='{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
sse_out="$tmp_dir/sse.out"
curl -sN "${auth_args[@]}" \
  -H "Accept: text/event-stream" \
  -H "Mcp-Session-Id: $session_id" \
  "$MCP_URL" > "$sse_out" &
sse_pid=$!
sleep 0.2

curl -sS -o "$tmp_dir/list.json" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session_id" \
  "${auth_args[@]}" \
  -X POST "$MCP_URL" \
  -d "$list_payload"

sleep 0.2

if [[ ! -f "$EVENT_DB" ]]; then
  echo "Event store not found at $EVENT_DB. Enable replay with SPARK_MCP_HTTP_RESUME_MODE=replay and SPARK_MCP_HTTP_EVENT_STORE=sqlite." >&2
  exit 1
fi

event_id="$(sqlite3 "$EVENT_DB" "select event_id from mcp_events where stream_id like '${session_id}%' order by created_at desc limit 1;")"
if [[ -z "$event_id" ]]; then
  echo "No event_id found in sqlite event store for session $session_id" >&2
  kill "$sse_pid" 2>/dev/null || true
  wait "$sse_pid" 2>/dev/null || true
  exit 1
fi

kill "$sse_pid" 2>/dev/null || true
wait "$sse_pid" 2>/dev/null || true
index="$event_id"
req=""
if [[ "$event_id" == */* ]]; then
  index="${event_id%%/*}"
  req="${event_id#*/}"
fi
if ! [[ "$index" =~ ^-?[0-9]+$ ]]; then
  echo "Unexpected event id format: $event_id" >&2
  exit 1
fi
prev_index=$((index - 1))
last_event_id="$prev_index"
if [[ -n "$req" && "$req" != "$event_id" ]]; then
  last_event_id="${prev_index}/${req}"
fi

curl -sS -o /dev/null \
  -H "Mcp-Session-Id: $session_id" \
  "${auth_args[@]}" \
  -X DELETE "$MCP_URL" || true

replay_out="$tmp_dir/replay.out"
timeout 5s curl -sN "${auth_args[@]}" \
  -H "Accept: text/event-stream" \
  -H "Mcp-Session-Id: $session_id" \
  -H "Last-Event-ID: $last_event_id" \
  "$MCP_URL" > "$replay_out" || true

if ! grep -q '^id:' "$replay_out"; then
  echo "Replay did not return events." >&2
  echo "Event ID: $event_id" >&2
  echo "Last-Event-ID: $last_event_id" >&2
  exit 1
fi

echo "Replay smoke test ok (session $session_id, event $event_id, last-event-id $last_event_id)"
