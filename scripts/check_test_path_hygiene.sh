#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

echo "[path-hygiene] checking trust-lsp tests for Windows-sensitive path patterns"

search_with_line_numbers() {
  local pattern="$1"
  local file="$2"
  if command -v rg >/dev/null 2>&1; then
    rg -n "$pattern" "$file"
  else
    grep -nE "$pattern" "$file"
  fi
}

hits_1="$(mktemp)"
hits_2="$(mktemp)"
trap 'rm -f "${hits_1}" "${hits_2}"' EXIT

if search_with_line_numbers 'repo[[:space:]]*=[[:space:]]*repo\.to_string_lossy\(\)' crates/trust-lsp/src/config.rs >"${hits_1}"; then
  echo "[path-hygiene] FAIL: raw repo.to_string_lossy() detected in TOML fixture formatting."
  echo "[path-hygiene] Use toml_git_source(&repo) when writing git path dependencies in tests."
  cat "${hits_1}"
  exit 1
fi

if search_with_line_numbers 'path[[:space:]]*==[[:space:]]*dep_source' crates/trust-lsp/src/handlers/tests/core.rs >"${hits_2}"; then
  echo "[path-hygiene] FAIL: direct dependency PathBuf equality detected in workspace symbol test."
  echo "[path-hygiene] Use normalize_path_for_assert() with canonicalized paths."
  cat "${hits_2}"
  exit 1
fi

if ! search_with_line_numbers 'fn[[:space:]]+toml_git_source[[:space:]]*\(' crates/trust-lsp/src/config.rs >/dev/null; then
  echo "[path-hygiene] FAIL: missing toml_git_source() helper in config tests."
  exit 1
fi

if ! search_with_line_numbers 'fn[[:space:]]+normalize_path_for_assert[[:space:]]*\(' crates/trust-lsp/src/handlers/tests/core.rs >/dev/null; then
  echo "[path-hygiene] FAIL: missing normalize_path_for_assert() helper in core handler tests."
  exit 1
fi

echo "[path-hygiene] PASS"
