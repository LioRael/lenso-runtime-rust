#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")" && pwd)"
args=()
node "$root/node_modules/@lenso/workers-runtime/build.mjs" --manifest "$root/Cargo.toml" --package lenso-workers-g2-host --out-dir "$root/pkg" "${args[@]}" "$@"
