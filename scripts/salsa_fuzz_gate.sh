#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-smoke}"
FUZZ_DIR="${SALSA_FUZZ_DIR:-fuzz}"
SMOKE_SECONDS="${SALSA_FUZZ_SMOKE_SECONDS:-30}"
EXTENDED_SECONDS="${SALSA_FUZZ_EXTENDED_SECONDS:-300}"
RSS_LIMIT_MB="${SALSA_FUZZ_RSS_LIMIT_MB:-4096}"

if [[ ! -d "$FUZZ_DIR" ]]; then
  echo "[salsa-fuzz] FAIL: fuzz dir not found at ${FUZZ_DIR}"
  exit 1
fi

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "[salsa-fuzz] FAIL: cargo-fuzz is required (cargo install cargo-fuzz)"
  exit 1
fi

case "$MODE" in
  smoke)
    MAX_TOTAL_TIME="$SMOKE_SECONDS"
    ;;
  extended)
    MAX_TOTAL_TIME="$EXTENDED_SECONDS"
    ;;
  *)
    echo "Usage: $0 [smoke|extended]"
    exit 1
    ;;
esac

run_target() {
  local target="$1"
  echo "[salsa-fuzz] running target=${target} mode=${MODE} max_total_time=${MAX_TOTAL_TIME}s"
  (
    cd "$FUZZ_DIR"
    cargo +nightly fuzz run "$target" -- \
      -max_total_time="${MAX_TOTAL_TIME}" \
      -rss_limit_mb="${RSS_LIMIT_MB}" \
      -verbosity=0 \
      -timeout=10 \
      -max_len=4096
  )
}

run_target "syntax_parse"
run_target "hir_semantic"

echo "[salsa-fuzz] PASS"
