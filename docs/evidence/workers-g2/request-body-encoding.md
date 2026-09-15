# W06 request body encoding: implementation and validation

Date: 2026-09-15. **Qualification blocked; acceptance is not complete.**
Base commit preserved: `40dc19b3114f7fd75874c764e131c2a576ee5363`.
Review comparison: `origin/main` at
`9fbed717aa1a5f7a31d56cf993877be58cdc3d0f` (reconciled W01).
All follow-up changes are uncommitted.

## Implementation and full branch review

Reviewed the entire branch against `origin/main`, including the partial codec
commit, both lockfiles, the Rust Host extraction and high-level Host option.

- Both HTTP handlers now validate `requestBodyEncoding` at construction and
  call the codec inside `prepareRequest`, after the W01 Runner has reserved
  shared capacity and the bounded body read succeeds. No body conversion runs
  when admission fails. The high-level Host forwards the same option.
- Default and explicit `numeric-array` serialize the same field order and bytes
  as W01. `base64-v1` is explicit opt-in, with canonical padded standard base64,
  fixed-size conversion chunks and an encoded-length bound.
- Both Rust entry points share the partial commit's strict decoder. Native
  tests confirm null/duplicate/unknown/ambiguous fields, malformed alphabets,
  padding and trailing bits are rejected. The serialized envelope bound remains
  `65536 * 4 + 16384 * 6`; decoded request bodies remain limited to 64 KiB.
  Web Ingress still applies its configured body/head limits. Response encoding,
  response limits and public error mappings are unchanged.
- Fixed the partial G2 harness's published Runner import: qualification now uses
  the current W01 Runner and transport together. The G2 default remains numeric;
  a build-time define enables base64 for both existing Worker entry points.
  Its streaming bridge enforces the existing Rust 64 KiB body boundary too.
- Reused every W01 transport-admission regression for both encodings, adding
  checks that overload acquires no reader, pulls no body, invokes no base64
  encoder and serializes no request envelope. Added exact envelope, multibyte,
  binary, chunk-boundary, 64 KiB, head-limit and construction-option coverage
  across buffered, streaming and high-level Hosts.
- Added an actual G2 Wasm suite for both encodings, real held-session admission,
  malformed Rust envelopes, bounded errors and recovery. Its execution is
  blocked below; it is not counted as passing evidence.

## Validation evidence

| Check | Result |
| --- | --- |
| Fixed `npm test --workspace packages/workers-runtime` from worktree root | **Blocked**, exit 254: root `package.json` does not exist. No root file was added outside allowed paths. |
| Supplementary `npm test --prefix packages/workers-runtime` | **Passed: 107 tests**, no failures, skips or cancellations. Runs the package's unchanged `node --test test/*.test.mjs` script; does not substitute for the fixed command. |
| Fixed `/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo check --locked --manifest-path experiments/workers-g2/Cargo.toml --target wasm32-unknown-unknown` | **Blocked**, exit 101: crates.io `config.json` unavailable through configured proxy `127.0.0.1:7890`. |
| Same locked G2 check with `CARGO_NET_OFFLINE=true` | **Blocked**, exit 101: `lenso-web-duplex-fixture` absent from the registry index. The G2 lock records local Web fixture packages, while the manifest no longer supplies their local cohort patches. |
| Locked native decoder tests, Rust 1.94.0, offline | **Passed: 7 tests** compiling the exact `host/src/request.rs`. No skips or failures. |
| `pnpm install --frozen-lockfile --ignore-scripts --offline` in G2 | **Blocked**, exit 1: frozen lock lacks declared `@lenso/workers-runtime@0.1.0` and `@lenso/web-ingress-workers@0.1.0`. Lock was not regenerated. |
| Real G2 build through the current package build CLI, Rust 1.94.0, locked, offline | **Blocked**, exit 1 / Cargo 101: missing `lenso-web-duplex-fixture`. No Wasm artifact produced. |
| `node --test experiments/workers-g2/request-body-wasm.test.mjs` | **Blocked**, exit 1: missing freshly generated `pkg/lenso_workers_g2_host.js`. Zero passing tests; not replaced with mocks or skipped. |
| Rust formatting and `git diff --check origin/main` | Passed. |

The sandbox denies writes to the shared Cargo target and denies sccache execution.
Supplementary native testing and the G2 build attempt still used `lenso-cargo`,
with `LENSO_CARGO_CACHE_ROOT="$PWD/experiments/workers-g2/.cargo-targets"` and
`RUSTC_WRAPPER=/usr/bin/env`. The temporary target directory was removed after
validation. The exact native command was:

```sh
LENSO_CARGO_CACHE_ROOT="$PWD/experiments/workers-g2/.cargo-targets" \
RUSTC_WRAPPER=/usr/bin/env \
  /Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo +1.94.0 \
  test --locked --offline \
  --manifest-path experiments/workers-g2/request-body-tests/Cargo.toml
```

## Coordinator decisions and remaining work

1. Supply the intended root npm workspace metadata, or revise the fixed command
   through coordination. Adding a root manifest is outside this task's scope.
2. Restore/identify the approved immutable Web fixture cohort and matching Cargo
   patch configuration, resolve the G2 frozen JS lock mismatch, and provide usable
   registry/cache access. This executor did not select alternate fixture versions,
   alter lock requirements or replace the published portable core.
3. Rerun both fixed checks, build G2 and execute the new actual-Wasm suite, then
   the existing local G2/workerd suites for both encodings using the
   [G2 commands](../../../experiments/workers-g2/README.md). Real Wasm/workerd
   qualification and bounded malformed-envelope HTTP behavior remain unproven
   by this run. Remove generated artifacts after qualification.

No deployment, publication, merge, commit, message delivery or worktree cleanup
was performed. No performance or Cloudflare platform claim follows from the
passing JavaScript/native decoder tests.

## Changed paths

Uncommitted follow-up paths:

- `packages/workers-runtime/http.mjs`
- `packages/workers-runtime/README.md`
- `packages/workers-runtime/test/http.test.mjs`
- `packages/workers-runtime/test/streaming.test.mjs`
- `packages/workers-runtime/test/transport-admission.mjs`
- `packages/workers-runtime/test/request-body.test.mjs`
- `experiments/workers-g2/runner.mjs`
- `experiments/workers-g2/README.md`
- `experiments/workers-g2/request-body-tests/src/lib.rs`
- `experiments/workers-g2/request-body-wasm.test.mjs`
- `docs/evidence/workers-g2/request-body-encoding.md`

Additional paths in the reviewed partial commit:

- `packages/workers-runtime/host.mjs`
- `packages/workers-runtime/request-body.mjs`
- `experiments/workers-g2/Cargo.lock`
- `experiments/workers-g2/host/Cargo.toml`
- `experiments/workers-g2/host/src/lib.rs`
- `experiments/workers-g2/host/src/request.rs`
- `experiments/workers-g2/host/src/sessions.rs`
- `experiments/workers-g2/request-body-tests/Cargo.toml`
- `experiments/workers-g2/request-body-tests/Cargo.lock`
