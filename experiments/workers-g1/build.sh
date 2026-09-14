#!/usr/bin/env bash
set -euo pipefail
: "${CARGO:=cargo}"
: "${WASM_BINDGEN:=wasm-bindgen}"
root="$(cd "$(dirname "$0")" && pwd)"
if [[ "$("$WASM_BINDGEN" --version)" != 'wasm-bindgen 0.2.127' ]]; then
  echo 'Install the locked wasm-bindgen-cli 0.2.127 first.' >&2
  exit 1
fi
receipt="$(mktemp)"
trap 'rm -f "$receipt"' EXIT
"$CARGO" +1.94.0 rustc --locked --manifest-path "$root/Cargo.toml" \
  --target wasm32-unknown-unknown -p lenso-workers-g1-host --release \
  --message-format=json -- -C link-arg=--export=__wasm_call_ctors > "$receipt"
artifact="$(python3 - "$receipt" <<'PY'
import json,sys
for line in open(sys.argv[1]):
    item=json.loads(line)
    if item.get('reason')=='compiler-artifact' and item['target']['name']=='lenso_workers_g1_host':
        for name in item['filenames']:
            if name.endswith('.wasm'):print(name)
PY
)"
[[ -f "$artifact" ]]
"$WASM_BINDGEN" "$artifact" --target web --out-dir "$root/pkg"
