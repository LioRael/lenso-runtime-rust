#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")" && pwd)"
node "$root/../../packages/workers-runtime/build.mjs" --manifest "$root/Cargo.toml" --package lenso-workers-g2-host --out-dir "$root/pkg" "$@"
