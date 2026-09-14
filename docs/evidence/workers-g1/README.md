# Workers G1 prototype evidence — 2026-09-14

This is the initial prototype report. See the [recovery follow-up](recovery.md)
for the async fault fix, conformance subset, and resource observations.
The subsequent [I/O and platform report](io-and-termination.md) records real
subrequests and CPU-limit termination.

Initial status: local and deployed generic runtime prototype passed. **G1 qualification
remains open** for asynchronous traps, forced termination/recreation, full Runtime
conformance, CPU measurement, and peak memory measurement. G2 HTTP parity and
product migrations are not approved by these results.

## Reproduction and pinned inputs

Source: [experiment](../../../experiments/workers-g1/README.md), based on Runtime
commit `1de9b1c`. The independent Cargo and pnpm lockfiles pin the closure.
Kernel 0.3.6, App Plan 0.4.1, HTTP Endpoint 0.3.1; the Native Adapter and macros
come from this repository. Rust 1.94.0, wasm-bindgen 0.2.127, Wrangler 4.107.0,
compatibility date 2026-07-08. The newer requested date initially failed local
workerd startup; the selected date is supported by the pinned local toolchain.

Validated commands (Cargo uses the workspace `lenso-cargo` wrapper):

```sh
CARGO=/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo \
  bash experiments/workers-g1/build.sh
/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo +1.94.0 clippy \
  --locked --manifest-path experiments/workers-g1/Cargo.toml \
  --target wasm32-unknown-unknown --workspace --no-deps -- -D warnings
WORKERS_G1_URL=http://127.0.0.1:63733 node experiments/workers-g1/smoke.mjs
WORKERS_G1_URL=https://lenso-workers-g1-proof.lenso.workers.dev \
  node experiments/workers-g1/smoke.mjs
```

Release build, Rust formatting, Clippy and both smoke runs passed. This does not
replace the complete Runtime conformance suite. Production source was unchanged.

## Observed behavior

- Exactly two generated factories enter the existing catalog and resolver.
- Both Plugin instances start; Kernel reaches Ready and accepts invocation.
- Host invokes the existing Endpoint operation through Caller's resolved binding.
- Twelve concurrent distinct Unicode inputs echo correctly; each new Echo has
  invocation count one. This proves request App state separation, not placement
  of all remote requests on a single isolate.
- Timers do not finish before their deadline; a sleeping task cancels.
- Pre-cancelled and expired contexts return the exact Cancelled and
  DeadlineExceeded failures, without entering the endpoint.
- 128 pending tasks are admitted; the next is rejected. Shutdown wakes the
  parked event lane, cancels all handles, and rejects new tasks.
- Missing factory and prepare failure reject startup before Ready.
- Ordinary successful requests await Clean shutdown. A subsequent request
  starts a fresh App, including after diagnostic failures.
- A synchronous Wasm unreachable trap returns HTTP 500, not success. This trap
  runs outside an active App and proves only the synchronous JS exception boundary.

Initial empty fixture linkage functions produced zero registered factories.
Calling the existing macro-generated explicit anchors fixed this. Exporting and
calling Wasm constructors alone did not fix that fixture. Automatic inventory
retention without anchors is therefore not certified.

## Deployed artifact

- Endpoint: https://lenso-workers-g1-proof.lenso.workers.dev/probe?input=hello
- Worker version: `f5df7d4e-9d47-4fda-9a3f-c4e52c05e2d1`
- Generated Wasm SHA-256: `44e2a199af5beafc3091e1846b7571808e52b5bc56c4e3224d8372460a989922`
- Wrangler total upload: 1313.47 KiB; gzip: 398.57 KiB.
- Wrangler-reported startup: 1 ms. This is not request latency or CPU time.
- One remote normal response: 1,245,184 bytes linear memory capacity and 5 ms
  elapsed event time. These are not peak memory or CPU measurements.

Wrangler emits duplicate-object-key warnings in generated wasm-bindgen import
glue for the inline JS module. Deployment and checks pass; generated files were
not edited. This toolchain warning remains to resolve before package promotion.

## Required before promotion

1. Exercise traps inside async execution and establish how the Host prevents
   abandoned task state from affecting later events. `catch_unwind` does not
   establish Wasm trap cleanup; destructors are not guaranteed on forced termination.
2. Verify actual isolate recreation/termination rather than just App recreation.
3. Run the applicable Runtime conformance corpus, including request-owned I/O.
4. Capture real CPU and peak memory under representative concurrent load.
5. Move proven Driver/Runner mechanics into supported Runtime packages only after
   those gates; then implement Web-owned HTTP parity. This GET diagnostic wrapper
   is not an alternative public ingress contract.

Auth, storage, signatures, streaming, client-disconnect cancellation, and
Marketplace production are outside this prototype. The existing Marketplace
domain and deployment were not changed.
