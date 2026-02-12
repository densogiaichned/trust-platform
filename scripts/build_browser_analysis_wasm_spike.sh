#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${ROOT_DIR}/target/browser-analysis-wasm"
PKG_DIR="${OUT_DIR}/pkg"
WEB_SRC="${ROOT_DIR}/docs/internal/prototypes/browser_analysis_wasm_spike/web"
WEB_OUT="${OUT_DIR}/web"

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "error: wasm-pack is required."
  echo "install: cargo install wasm-pack"
  exit 1
fi

if command -v rustup >/dev/null 2>&1; then
  rustup target add wasm32-unknown-unknown >/dev/null
fi

rm -rf "${PKG_DIR}" "${WEB_OUT}"
mkdir -p "${PKG_DIR}" "${WEB_OUT}"

(
  cd "${ROOT_DIR}/crates/trust-wasm-analysis"
  wasm-pack build \
    --target web \
    --out-dir "${PKG_DIR}" \
    --out-name trust_wasm_analysis \
    -- \
    --features wasm
)

cp "${WEB_SRC}/index.html" "${WEB_OUT}/index.html"
cp "${WEB_SRC}/main.js" "${WEB_OUT}/main.js"
cp "${WEB_SRC}/worker.js" "${WEB_OUT}/worker.js"

cat <<EOF
WASM browser analysis spike build complete.
- Package: ${PKG_DIR}
- Host: ${WEB_OUT}

To run locally:
  python3 -m http.server 4173 --directory "${WEB_OUT}"
Then open:
  http://127.0.0.1:4173/
EOF
