# Workers G1 experiment

This isolated workspace proves a bounded Lenso App on Cloudflare Workers. It is
not a supported Runtime package, production HTTP Ingress, or Marketplace backend.
See [recorded evidence](../../docs/evidence/workers-g1/README.md) and the
[target design](../../docs/design/cloudflare-workers-target.md).

The existing Plugin Root resolver builds the plan. Two macro-authored Plugins
use the existing Native Adapter and HTTP Endpoint contract. The Host invokes
Echo through Caller's resolved requirement; Caller does not implement a second
business operation. Every request creates a Driver and App, invokes the endpoint,
and awaits clean shutdown. Immutable registrations and the Wasm module instance
are isolate-scoped; Plugin execution state is request-scoped.

Generated `__lenso_link_*` anchors retain the Plugin factories. The Worker also
runs exported Wasm constructors once after module initialization. No descriptors
or generated bindings are hand-edited. WorkersDriver is an experimental adaptation
of BrowserDriver with Workers clock/timer imports and bounded event-owned tasks.

## Reproduce

Requirements: Rust 1.94.0 with `wasm32-unknown-unknown`, wasm-bindgen-cli 0.2.127,
Python 3, Node, and pnpm. Wrangler is locked to 4.107.0 with compatibility date
2026-07-08. Run from this directory:

```sh
pnpm install --frozen-lockfile
# In the Lenso workspace, CARGO must point to .lenso-tools/bin/lenso-cargo.
bash build.sh
pnpm exec wrangler dev --port 63733
```

In another terminal:

```sh
WORKERS_G1_URL=http://127.0.0.1:63733 node smoke.mjs
```

For the authorized experimental deployment, with Wrangler authenticated:

```sh
pnpm exec wrangler deploy
WORKERS_G1_URL=https://lenso-workers-g1-proof.lenso.workers.dev node smoke.mjs
```

The endpoint is `GET /probe?input=hello`. Diagnostic modes are `normal`,
`missing-factory`, `startup-failure`, `driver`, `conformance`, `trap`,
`async-trap`, `task-trap`, `async-panic`, `async-pending`, and `recovery`. Input is bounded;
there are no production bindings, signing keys, or mutations. The deployment
is a disposable experiment with a 1000 ms CPU limit, not a public product API.

`wasm_memory_bytes` reports linear memory capacity, not peak process memory.
`elapsed_ms` reports event elapsed time including timers, not CPU time.
The smoke test deliberately triggers synchronous and asynchronous traps and a
Plugin panic. These return 500 with shutdown unconfirmed. The Runner bounds
admission to 32 events and applies a separate 1000 ms event deadline. Any trapped,
rejected, or expired generation rejects its in-flight events, clears its host
timers, and reinitializes Wasm using generated wasm-bindgen reset support. Failure
to reinitialize keeps admission closed. A reset affects all in-flight events in
that Wasm instance; it does not resume Plugin execution or roll back durable writes.

The `recovery` probe runs two failed invocations and a successful invocation inside
one fetch. It verifies matching abandoned generations and a different Wasm memory
object after reset. This proves Wasm instance recreation, not V8 isolate eviction.
A CPU-bound loop can prevent the JS event timer from firing; platform CPU limits
remain the preemptive boundary.

For reproducible local V8 CPU and heap sampling, with the local Worker running
on 63733 and its inspector on 9229:

```sh
pnpm run profile
```

The script checks 240 requests at concurrency 12 and writes a CPU profile to
`/tmp/workers-g1-profile.cpuprofile` (override `WORKERS_G1_PROFILE_PATH`). JSON
reports batch-boundary heap observations and CPU samples, not remote billed CPU
or an absolute peak process memory measurement. See the
[recovery follow-up](../../docs/evidence/workers-g1/recovery.md) for evidence.
