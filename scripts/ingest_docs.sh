#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORPUS_DIR="${SPARK_MCP_CORPUS_DIR:-"$ROOT_DIR/corpus"}"
TMP_DIR="${SPARK_MCP_TMP_DIR:-"$ROOT_DIR/.tmp"}"
INCLUDE_LIVE_WAVE_DOCS="${SPARK_MCP_INCLUDE_LIVE_WAVE_DOCS:-0}"
LIVE_WAVE_IDS="${SPARK_MCP_LIVE_WAVE_IDS:-spark2014}"
INCLUDE_GNATPROVE_DIAGNOSTICS="${SPARK_MCP_INCLUDE_GNATPROVE_DIAGNOSTICS:-1}"
INCLUDE_WHY3_BRIDGE="${SPARK_MCP_INCLUDE_WHY3_BRIDGE:-0}"

mirror_html_tree() {
  local url="$1"
  local target_dir="$2"
  local cut_dirs="$3"

  mkdir -p "$target_dir"
  wget -r -l inf -np -A html,htm -e robots=off \
    --no-host-directories --cut-dirs="$cut_dirs" \
    -P "$target_dir" \
    "$url"
}

is_truthy() {
  local value="${1,,}"
  [[ "$value" == "1" || "$value" == "true" || "$value" == "yes" || "$value" == "on" ]]
}

mkdir -p "$CORPUS_DIR/spark-reference-manual/raw"
mkdir -p "$CORPUS_DIR/spark-user-guide/raw"
mkdir -p "$CORPUS_DIR/ada-reference-manual/text"
mkdir -p "$CORPUS_DIR/ada-reference-manual/html"
mkdir -p "$CORPUS_DIR/ada-2022-overview"
mkdir -p "$CORPUS_DIR/ada-2022-aarm/html"
mkdir -p "$TMP_DIR"

if [[ -f "$CORPUS_DIR/spark-reference-manual/spark-reference-manual_index.html" ]]; then
  mv "$CORPUS_DIR/spark-reference-manual/spark-reference-manual_index.html" \
    "$CORPUS_DIR/spark-reference-manual/raw/index.html"
fi

# SPARK Reference Manual (HTML)
mirror_html_tree \
  "https://docs.adacore.com/spark2014-docs/html/lrm/" \
  "$CORPUS_DIR/spark-reference-manual/raw" \
  "3"

# SPARK User's Guide (GNATprove + flow analysis) (HTML)
mirror_html_tree \
  "https://docs.adacore.com/spark2014-docs/html/ug/" \
  "$CORPUS_DIR/spark-user-guide/raw" \
  "3"

if is_truthy "$INCLUDE_LIVE_WAVE_DOCS"; then
  IFS=',' read -r -a waves <<<"$LIVE_WAVE_IDS"
  for wave_raw in "${waves[@]}"; do
    wave="$(echo "$wave_raw" | tr -d '[:space:]')"
    if [[ -z "$wave" ]]; then
      continue
    fi
    if [[ ! "$wave" =~ ^[A-Za-z0-9._-]+$ ]]; then
      echo "Invalid SPARK live/wave id: '$wave' (allowed: letters, digits, . _ -)" >&2
      exit 1
    fi

    mirror_html_tree \
      "https://docs.adacore.com/live/wave/${wave}/html/${wave}_rm/" \
      "$CORPUS_DIR/spark-reference-manual-live-${wave}/raw" \
      "5"

    mirror_html_tree \
      "https://docs.adacore.com/live/wave/${wave}/html/${wave}_ug/" \
      "$CORPUS_DIR/spark-user-guide-live-${wave}/raw" \
      "5"
  done
fi

if is_truthy "$INCLUDE_GNATPROVE_DIAGNOSTICS"; then
  DIAG_SOURCE_DIR="$CORPUS_DIR/spark2014/share/spark/explain_codes"
  DIAG_TARGET_DIR="$CORPUS_DIR/spark-gnatprove-diagnostics/raw"

  if [[ -d "$DIAG_SOURCE_DIR" ]]; then
    mkdir -p "$DIAG_TARGET_DIR"
    find "$DIAG_TARGET_DIR" -maxdepth 1 -type f -name '*.md' -delete
    copied=0
    while IFS= read -r -d '' md_file; do
      cp "$md_file" "$DIAG_TARGET_DIR/"
      copied=$((copied + 1))
    done < <(find "$DIAG_SOURCE_DIR" -maxdepth 1 -type f -name '*.md' -print0 | sort -z)
    echo "GNATprove diagnostics copied: $copied file(s) into $DIAG_TARGET_DIR"
  else
    echo "GNATprove diagnostics source missing at $DIAG_SOURCE_DIR; skipping diagnostics slice" >&2
  fi
fi

if is_truthy "$INCLUDE_WHY3_BRIDGE"; then
  WHY3_DOCS_URL="https://www.why3.org/doc/"
  WHY3_TARGET_DIR="$CORPUS_DIR/why3-reference/raw"
  mirror_html_tree "$WHY3_DOCS_URL" "$WHY3_TARGET_DIR" "1"
fi

# Ada 2022 Reference Manual (TXT)
curl -L \
  "https://www.adaic.org/resources/add_content/standards/22rm/RM-22-Txt.zip" \
  -o "$TMP_DIR/ada-rm-22-txt.zip"
unzip -q -o "$TMP_DIR/ada-rm-22-txt.zip" -d "$CORPUS_DIR/ada-reference-manual/text"
rm -f "$TMP_DIR/ada-rm-22-txt.zip"

# Ada 2022 Reference Manual (HTML)
curl -L \
  "https://www.adaic.org/resources/add_content/standards/22rm/RM-22-Html.zip" \
  -o "$TMP_DIR/ada-rm-22-html.zip"
unzip -q -o "$TMP_DIR/ada-rm-22-html.zip" -d "$CORPUS_DIR/ada-reference-manual/html"
rm -f "$TMP_DIR/ada-rm-22-html.zip"

# Ada 2022 Annotated Reference Manual (AARM) (HTML)
curl -L \
  "https://www.adaic.org/resources/add_content/standards/22aarm/AA-22-Html.zip" \
  -o "$TMP_DIR/ada-aarm-22-html.zip"
unzip -q -o "$TMP_DIR/ada-aarm-22-html.zip" -d "$CORPUS_DIR/ada-2022-aarm/html"
rm -f "$TMP_DIR/ada-aarm-22-html.zip"

# Ada 2022 Overview (HTML)
curl -L \
  "https://www.adaic.org/resources/add_content/standards/ada2022.html" \
  -o "$CORPUS_DIR/ada-2022-overview/ada-2022-overview.html"

if [[ -d "$CORPUS_DIR/spark2014" && ! -f "$CORPUS_DIR/.spark-mcp-ignore" ]]; then
  cat > "$CORPUS_DIR/.spark-mcp-ignore" <<'EOF'
# Reduce noisy corpus content.
spark2014/testsuite/
EOF
fi

echo "Docs fetched into $CORPUS_DIR"
