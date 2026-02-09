#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

ITERATIONS=8
if [[ "${1:-}" == "--iterations" ]]; then
  if [[ -z "${2:-}" ]]; then
    echo "Usage: $0 [--iterations <n>]"
    exit 2
  fi
  ITERATIONS="$2"
  shift 2
fi

if ! [[ "${ITERATIONS}" =~ ^[0-9]+$ ]] || [[ "${ITERATIONS}" -lt 1 ]]; then
  echo "[mesh-gate] invalid --iterations value: ${ITERATIONS}"
  exit 2
fi

echo "[mesh-gate] running mesh TLS publish regression ${ITERATIONS} time(s)"
for i in $(seq 1 "${ITERATIONS}"); do
  echo "[mesh-gate] run ${i}/${ITERATIONS}"
  cargo test -p trust-runtime --lib mesh::tests::mesh_tls_publish_applies_updates -- --nocapture
done
echo "[mesh-gate] PASS"
