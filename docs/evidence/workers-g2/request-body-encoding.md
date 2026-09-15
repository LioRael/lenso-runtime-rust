# W06 request body encoding qualification

Date: 2026-09-15. **Real Rust/Wasm qualification passed in Node and workerd.**

## Identities and scope

- Execution worktree: `perf-workers-byte-envelope`.
- Preserved task-start HEAD: `bf7b35014fbf56d6b008d1ad29ead42b0b5a374c`.
- Entire branch reviewed against `origin/main` at
  `9fbed717aa1a5f7a31d56cf993877be58cdc3d0f`, including both codecs, transport
  admission, the high-level Host, Rust entry points, tests, build wiring and locks.
- Canonical source owner: `LioRael/lenso-web`, exact commit
  `6611fd3af560d246e18d6e20d6963f68f1f07566`; its checkout was clean. Files were
  read with `git show <commit>:<path>`. No build consumes sibling source.
- Toolchain: `rustc 1.94.0 (4a4ef493e 2026-03-02)`, target
  `wasm32-unknown-unknown`, `wasm-bindgen 0.2.127`, Node `v26.8.2`, pnpm `11.5.0`,
  Wrangler `4.107.0`, workerd package `1.20260701.1` (`workerd 2026-07-01`),
  esbuild `0.28.1`, macOS arm64.

The [fixture provenance record](../../../experiments/workers-g2/fixtures/README.md)
lists source paths, exact SHA-256 hashes and manifest adaptations. Both Rust
providers and the 30-vector HTTP corpus are unchanged. The dependency-free
WebSocket adapter is copied unchanged from the same owner commit and matches
its copy in the released Web Ingress `0.4.3` crate.

The experiment pins released HTTP Endpoint `0.3.1`, HTTP Stream Endpoint `0.1.1`,
WebSocket Endpoint `0.1.0`, Web Ingress `0.4.3`, Kernel `0.3.6` and App Plan
`0.4.1`. Released Ingress `0.4.2` lacks the Wasm/duplex implementation described
by the previous local lock; `0.4.3` supplies it. Runtime facade `0.5.21`, native
adapter `0.3.14`, adapter macros `0.2.5` and Workers Driver `0.1.0` resolve within
this Runtime checkout. No production dependency or public Capability changed.

## Review finding and correction

Serde's externally tagged unit enum accepted
`"body_encoding":{"base64-v1":null}`. That violates the string-only transport
version contract. A new native regression first failed (7 passed, 1 failed);
requiring `Option<String>` and matching the literal `"base64-v1"` fixed it.
The shared real-Wasm suite also rejects this object, arrays, numbers and booleans.

W01 admission ordering remains intact: both transports reserve shared capacity
before body acquisition, copying, encoding or serialization. The default and
explicit numeric representation preserve the legacy JSON field order and bytes.
Base64 remains opt-in, canonical padded standard base64, with 12,288-byte encoding
chunks, an 87,384-character bound and a 65,536-byte decoded bound. Both Rust entry
points use the same decoder; ambiguous, duplicate, null and unknown fields fail.
Response encoding, HTTP error mappings and the portable Capability contracts are
unchanged.

## Results

| Check | Result |
| --- | --- |
| Fixed `npm test --prefix packages/workers-runtime` | **107 passed**, 0 failed/skipped/cancelled |
| Fixed Rust 1.94 G2 `check --locked`, wasm32 target | **Passed**, exit 0 |
| Native decoder `test --frozen` | **8 passed**, 0 failed/ignored |
| Actual G2 release build and wasm-bindgen | **Passed**, exit 0 |
| `request-body-wasm.test.mjs` in Node | **8 passed**, 0 failed/skipped/cancelled |
| Same assertions through native `workerd test` | **8 passed**, service `request-body` passed |
| Clean source snapshot: frozen G2 check and release/bindgen build | **Passed**, exit 0 |
| Clean source snapshot: Node and workerd qualification | **8 + 8 passed** |
| Both Rust fmt checks; `git diff --check origin/main` | **Passed**, exit 0 |

The eight shared cases cover both representations with empty, binary and
multibyte bodies, lengths 1/2/3/256, either side of the 12,288-byte chunk boundary,
and 65,534/65,535/65,536 bytes. The Plan-bound HTTP provider returns exact bytes
and clean shutdown. The unchanged duplex provider streams `[0,1,255,2,3,254]`
and rejects POST with its existing 405 contract. A separate case directly
compares status, response headers (excluding the generated request ID) and bytes
between representations for both transports.

Real held sessions reject both transports with 503 before acquiring a reader,
pulling bytes or calling `btoa`, then allow a healthy request after cancellation.
Both transports preserve 413 body and 431 head failures. Malformed/noncanonical
base64, unsupported or non-string versions, missing/null/ambiguous/duplicate/
unknown fields, invalid numeric bytes, over-limit decoded and encoded lengths,
and oversized serialized envelopes enter **both actual Rust entry points**.
They return bounded 503 HTTP errors with `no-store`/`nosniff`; a real healthy
request follows every rejection. These are assertions against generated Wasm,
not mocked Rust results.

## Exact validation commands

Run from the Runtime root. The cache/ wrapper overrides below accommodate the
sandbox's read-only shared target and unavailable sccache; every Cargo invocation
still uses `lenso-cargo`. `CARGO_NET_OFFLINE=true` plus `--locked` prevents both
network resolution and lock mutation (equivalent to `--frozen`).

```sh
export LENSO_CARGO_CACHE_ROOT="$PWD/experiments/workers-g2/.cargo-targets"
export RUSTC_WRAPPER=/usr/bin/env
export CARGO_NET_OFFLINE=true
export CARGO=/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo

npm test --prefix packages/workers-runtime
/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo +1.94.0 \
  check --locked --manifest-path experiments/workers-g2/Cargo.toml \
  --target wasm32-unknown-unknown
/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo +1.94.0 \
  test --frozen --manifest-path experiments/workers-g2/request-body-tests/Cargo.toml
bash experiments/workers-g2/build.sh
node --test experiments/workers-g2/request-body-wasm.test.mjs
node experiments/workers-g2/qualify-request-body-workerd.mjs
/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo +1.94.0 \
  fmt --manifest-path experiments/workers-g2/Cargo.toml --all -- --check
/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo +1.94.0 \
  fmt --manifest-path experiments/workers-g2/request-body-tests/Cargo.toml -- --check
git diff --check origin/main
```

`build.sh` invokes this checkout's `packages/workers-runtime/build.mjs`, which
executes Rust `+1.94.0 rustc --locked --target wasm32-unknown-unknown --release
--no-default-features`, exports `__wasm_call_ctors`, and invokes wasm-bindgen
`--target web --experimental-reset-state-function`. The workerd harness bundles
the same assertion source with locked Wrangler's esbuild and invokes its workerd
binary with `test pkg/request-body-workerd.capnp`. Its compatibility date is
`2026-07-08`; `nodejs_compat` supplies assertions and Buffer in the test module.
Production Worker compatibility flags are unchanged.

### Clean source proof

Created `/tmp/w06-clean-runtime` from `git archive HEAD` and overlaid the current
scoped source files, including the new fixtures and harness. The snapshot
contained no `pkg`, `target`, `node_modules`, `.cargo-targets`, `.pnpm-store` or
`.wrangler` artifacts. It is the proposed source tree, without worktree metadata
or a sibling checkout. With the same environment and original worktree cwd:

```sh
/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo +1.94.0 \
  check --locked \
  --manifest-path /tmp/w06-clean-runtime/experiments/workers-g2/Cargo.toml \
  --target wasm32-unknown-unknown
bash /tmp/w06-clean-runtime/experiments/workers-g2/build.sh
node --test /tmp/w06-clean-runtime/experiments/workers-g2/request-body-wasm.test.mjs
node /tmp/w06-clean-runtime/experiments/workers-g2/qualify-request-body-workerd.mjs
```

For those final JS commands, the snapshot's Runtime package link pointed to its
own `packages/workers-runtime`; Wrangler reused the verified cached tooling
below. Cargo output resolved all local crates and fixtures inside the snapshot,
and all portable/Web crates from the registry cache with the frozen lock.

### JS lock and sandbox setup

The pnpm lock now includes the same-checkout Runtime link required by the
Driver's generated clock import. Unresolved published fixture/transport imports
were removed. All pre-existing Wrangler/ws versions and integrity entries remain
unchanged. Normal clean setup is:

```sh
pnpm --dir experiments/workers-g2 install --frozen-lockfile --ignore-scripts
```

In this sandbox, the offline form validated the manifest against the frozen lock
(`Lockfile is up to date, resolution step is skipped`) and created the dependency
layout, then lacked the pnpm esbuild tarball. Registry/proxy access was unavailable.
All **36 platform-selected dependency tarballs** were available in the local npm
cache. They were restored into that layout after checking each SHA-512 against
its exact entry in the unchanged pnpm lock; Runtime linked to this checkout.
No package version, integrity check, assertion or test was relaxed. The executed
restoration command was `node /tmp/w06-hydrate.mjs`; its SHA-256 was
`39fad273ad68a07c91da10866dd175e8bf28a12df75c42bfa97e844994dcfc71`.
A later pnpm policy-metadata verification could not access the registry and was
terminated; a complete network-backed pnpm installation is not claimed.

The artifact identities below and successful real workerd execution apply to
this locked, integrity-verified tool cohort. Registry access remains an
environment prerequisite for a fresh machine without cached dependencies.

## Artifact identities

SHA-256, recorded before generated artifacts were removed:

| Artifact | SHA-256 |
| --- | --- |
| G2 `Cargo.lock` | `a70a35bf3b4b6ef3bb2745ea946178a889feadd7f5483648b46170d4811ddcc7` |
| G2 `pnpm-lock.yaml` | `0336e36a8aa8a0c3b03a999dd6587d1593c66af113752662d95369dac393aaf3` |
| Decoder `Cargo.lock` | `7371588c950df2e1248c57ae8c065a627a76384b8507d5647986a2f0806765c0` |
| Worktree `lenso_workers_g2_host_bg.wasm` | `851b8dd3ed013d14d116b31614f44117e361390b62f027093a6be1511f95a34a` |
| Worktree generated JS | `5740343c29a7e0d5f4a7f192b95b317f740a8e39f61ade879502c4632fb2c4ad` |
| Clean snapshot `lenso_workers_g2_host_bg.wasm` | `4d4b37243a96d93b72853247b268c30bf4a018c5bee4e8387e2bf06a73b69060` |
| Clean snapshot generated JS | `946be39fab255e05a241e956119abf06cfe74b8d1df8b9e68df60e1cbf3d964f` |

Both builds executed successfully. This establishes source/build reproducibility;
artifact bytes differ across source locations and are not claimed deterministic.

## Limits and final state

Wrangler's local HTTP listener attempt failed with `listen EPERM ... 127.0.0.1`.
Workerd's native test mode executed the real Fetch bridge and Rust/Wasm host
successfully without a listener. The existing network smoke/limits/WebSocket and
Cloudflare disconnect suites were not rerun; no new edge-disconnect, performance,
full-isolate memory or deployment claim follows from W06.

All task changes remain uncommitted, with the task-start HEAD preserved. Generated
`pkg`, Cargo targets, `node_modules`, `.wrangler` and temporary package-store
artifacts were removed after validation. No merge, publication, deployment,
message delivery or worktree cleanup was performed.

## Changed paths in this execution

These 26 paths are uncommitted relative to the preserved task-start HEAD.
The existing `packages/workers-runtime` branch changes were reviewed and tested;
this execution needed no further edits there.

- `docs/evidence/workers-g2/request-body-encoding.md`
- `experiments/workers-g2/Cargo.lock`
- `experiments/workers-g2/Cargo.toml`
- `experiments/workers-g2/README.md`
- `experiments/workers-g2/build.sh`
- `experiments/workers-g2/cancellation.mjs`
- `experiments/workers-g2/clock.mjs`
- `experiments/workers-g2/fixtures/README.md`
- `experiments/workers-g2/fixtures/duplex-endpoint-plugin/Cargo.toml`
- `experiments/workers-g2/fixtures/duplex-endpoint-plugin/src/lib.rs`
- `experiments/workers-g2/fixtures/http-parity-plugin/Cargo.toml`
- `experiments/workers-g2/fixtures/http-parity-plugin/corpus.json`
- `experiments/workers-g2/fixtures/http-parity-plugin/src/lib.rs`
- `experiments/workers-g2/fixtures/websocket.mjs`
- `experiments/workers-g2/host/Cargo.toml`
- `experiments/workers-g2/host/src/request.rs`
- `experiments/workers-g2/package.json`
- `experiments/workers-g2/pnpm-lock.yaml`
- `experiments/workers-g2/qualify-request-body-workerd.mjs`
- `experiments/workers-g2/request-body-qualification.mjs`
- `experiments/workers-g2/request-body-tests/Cargo.toml`
- `experiments/workers-g2/request-body-tests/src/lib.rs`
- `experiments/workers-g2/request-body-wasm.test.mjs`
- `experiments/workers-g2/request-body-workerd.mjs`
- `experiments/workers-g2/runner.mjs`
- `experiments/workers-g2/smoke.mjs`
