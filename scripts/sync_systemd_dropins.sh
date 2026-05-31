#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SERVERS_ROOT="$(cd "$REPO_ROOT/.." && pwd)"
WORKSPACE_ROOT="$(cd "$SERVERS_ROOT/.." && pwd)"
SERVICE_NAME="spark-mcp"
TEMPLATE_DIR="$REPO_ROOT/systemd/user/${SERVICE_NAME}.service.d"
SYSTEMD_USER_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
OUTPUT_DIR="$SYSTEMD_USER_DIR/${SERVICE_NAME}.service.d"

MODE="apply"
DO_RELOAD=0
DO_RESTART=0

usage() {
  cat <<'EOF'
sync_systemd_dropins.sh

Sync managed user-level systemd drop-ins for spark-mcp.

Usage:
  ./scripts/sync_systemd_dropins.sh [--check|--apply] [--reload] [--restart]

Options:
  --check    Check drift only; do not write files.
  --apply    Apply templates (default).
  --reload   Run `systemctl --user daemon-reload` after apply.
  --restart  Run daemon-reload and restart spark-mcp.service.
  -h, --help Show this help.
EOF
}

render_template() {
  local template="$1"
  sed \
    -e "s|@SERVERS_ROOT@|$SERVERS_ROOT|g" \
    -e "s|@WORKSPACE_ROOT@|$WORKSPACE_ROOT|g" \
    "$template"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)
      MODE="check"
      shift
      ;;
    --apply)
      MODE="apply"
      shift
      ;;
    --reload)
      DO_RELOAD=1
      shift
      ;;
    --restart)
      DO_RELOAD=1
      DO_RESTART=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! -d "$TEMPLATE_DIR" ]]; then
  echo "missing template directory: $TEMPLATE_DIR" >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR"

drift_detected=0
change_detected=0
template_count=0

while IFS= read -r -d '' template_path; do
  template_count=$((template_count + 1))
  output_name="$(basename "$template_path" .in)"
  output_path="$OUTPUT_DIR/$output_name"
  rendered="$(mktemp)"
  render_template "$template_path" > "$rendered"

  if [[ "$MODE" == "check" ]]; then
    if [[ ! -f "$output_path" ]]; then
      echo "[drift] missing $output_path"
      drift_detected=1
    elif ! cmp -s "$rendered" "$output_path"; then
      echo "[drift] differs $output_path"
      diff -u "$output_path" "$rendered" || true
      drift_detected=1
    else
      echo "[ok] $output_path"
    fi
  else
    if [[ ! -f "$output_path" ]] || ! cmp -s "$rendered" "$output_path"; then
      install -m 0644 "$rendered" "$output_path"
      echo "[updated] $output_path"
      change_detected=1
    else
      echo "[ok] $output_path"
    fi
  fi

  rm -f "$rendered"
done < <(find "$TEMPLATE_DIR" -maxdepth 1 -type f -name '*.in' -print0 | sort -z)

if [[ "$template_count" -eq 0 ]]; then
  echo "no templates found in $TEMPLATE_DIR" >&2
  exit 1
fi

if [[ "$MODE" == "check" ]]; then
  if [[ "$drift_detected" -eq 1 ]]; then
    echo "managed drop-in drift detected for ${SERVICE_NAME}"
    exit 1
  fi
  echo "all managed drop-ins are in sync for ${SERVICE_NAME}"
  exit 0
fi

if [[ "$DO_RELOAD" -eq 1 ]]; then
  systemctl --user daemon-reload
  echo "[systemd] daemon-reload complete"
fi

if [[ "$DO_RESTART" -eq 1 ]]; then
  systemctl --user restart "${SERVICE_NAME}.service"
  echo "[systemd] restarted ${SERVICE_NAME}.service"
fi

if [[ "$change_detected" -eq 0 ]]; then
  echo "no managed drop-in changes were required for ${SERVICE_NAME}"
fi
