#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOTSTRAP_TOOLS=0
STRICT_OUTDATED="${STRICT_OUTDATED:-0}"
WORKSPACES=(".")

if [[ -d "${HOME}/.cargo/bin" ]]; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

usage() {
  cat <<'USAGE'
Usage: ./scripts/dependency_governance_check.sh [--bootstrap-tools]

Checks (per configured workspace):
  1) cargo deny   -> advisory/license/source policy (blocking)
  2) cargo audit  -> RustSec vulnerabilities (blocking)
  3) cargo outdated (direct deps) -> stale-risk report (non-blocking by default)

Env:
  STRICT_OUTDATED=0  Report outdated dependencies without failing (default)
  STRICT_OUTDATED=1  Fail if direct dependencies are outdated

Options:
  --bootstrap-tools  Install missing cargo subcommands with `cargo install --locked`
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bootstrap-tools)
      BOOTSTRAP_TOOLS=1
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

SCHED_PREFIX=()
if command -v ionice >/dev/null 2>&1; then
  SCHED_PREFIX+=(ionice -c3)
fi
if command -v nice >/dev/null 2>&1; then
  SCHED_PREFIX+=(nice -n 19)
fi

run_cmd() {
  if [[ ${#SCHED_PREFIX[@]} -gt 0 ]]; then
    "${SCHED_PREFIX[@]}" "$@"
  else
    "$@"
  fi
}

ensure_command() {
  local cmd="$1"
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    echo "missing required command: ${cmd}" >&2
    return 1
  fi
  return 0
}

ensure_cargo_subcommand_binary() {
  local binary="$1"
  local crate="$2"
  if command -v "${binary}" >/dev/null 2>&1; then
    return 0
  fi

  if [[ "${BOOTSTRAP_TOOLS}" -eq 1 ]]; then
    echo "installing ${crate} (missing ${binary})..." >&2
    run_cmd cargo install --locked "${crate}"
    return 0
  fi

  echo "missing ${binary}; install with: cargo install --locked ${crate}" >&2
  return 1
}

run_workspace_checks() {
  local workspace_dir="$1"
  local workspace_path="${ROOT_DIR}/${workspace_dir}"

  if [[ ! -d "${workspace_path}" ]]; then
    echo "workspace not found: ${workspace_dir}" >&2
    return 2
  fi
  if [[ ! -f "${workspace_path}/Cargo.toml" ]]; then
    echo "Cargo.toml not found in workspace: ${workspace_dir}" >&2
    return 2
  fi

  echo "[workspace: ${workspace_dir}] [1/3] cargo deny (advisories + licenses + bans + sources)"
  (
    cd "${workspace_path}"
    run_cmd cargo deny check advisories licenses bans sources
  )

  echo "[workspace: ${workspace_dir}] [2/3] cargo audit (RustSec)"
  (
    cd "${workspace_path}"
    run_cmd cargo audit --deny warnings \
      --ignore RUSTSEC-2024-0384 \
      --ignore RUSTSEC-2024-0436 \
      --ignore RUSTSEC-2025-0141 \
      --ignore RUSTSEC-2026-0002
  )

  echo "[workspace: ${workspace_dir}] [3/3] cargo outdated (direct dependency stale-risk)"
  if [[ "${STRICT_OUTDATED}" == "1" ]]; then
    (
      cd "${workspace_path}"
      run_cmd cargo outdated --root-deps-only --depth 1 --exit-code 1
    )
  else
    (
      cd "${workspace_path}"
      run_cmd cargo outdated --root-deps-only --depth 1 \
        || echo "cargo outdated report unavailable; continuing because STRICT_OUTDATED=0" >&2
    )
  fi
}

ensure_command cargo

missing_tools=0
ensure_cargo_subcommand_binary cargo-deny cargo-deny || missing_tools=1
ensure_cargo_subcommand_binary cargo-audit cargo-audit || missing_tools=1
ensure_cargo_subcommand_binary cargo-outdated cargo-outdated || missing_tools=1

if [[ "${missing_tools}" -ne 0 ]]; then
  echo "dependency governance check aborted due to missing tooling" >&2
  echo "tip: rerun with --bootstrap-tools" >&2
  exit 2
fi

for workspace_dir in "${WORKSPACES[@]}"; do
  run_workspace_checks "${workspace_dir}"
done

echo "dependency governance checks passed"
