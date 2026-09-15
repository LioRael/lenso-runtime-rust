#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")" && pwd)"
args=()
if [[ "${WORKERS_G1_CPU_PROBE:-0}" == 1 ]]; then args+=(--features cpu-probe); fi
node "$root/node_modules/@lenso/workers-runtime/build.mjs" --manifest "$root/Cargo.toml" --package lenso-workers-g1-host --out-dir "$root/pkg" "${args[@]}" "$@"
