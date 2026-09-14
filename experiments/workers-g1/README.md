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

## Request-owned I/O proof

The Rust Host receives an explicit JS I/O scope while its App is Ready. The scope
owns its fetch controller and bounded response reader; the Runner aborts it on
cancellation, completion, or generation abandonment. It is an experimental Host
facility, not a new Plugin Capability or an Auth transport implementation.

Run the separate stateless upstream in one terminal:

```sh
pnpm exec wrangler dev --config upstream/wrangler.jsonc --port 63734 --inspector-port 9230
```

Start the main experiment in another:

```sh
pnpm exec wrangler dev --port 63733 --var UPSTREAM_BASE:http://127.0.0.1:63734
```

Then run `WORKERS_G1_URL=http://127.0.0.1:63733 pnpm run smoke:io`.
The deployed upstream is `lenso-workers-g1-upstream`; the main experiment pins
its URL in `UPSTREAM_BASE` and enables `global_fetch_strictly_public` for public
Worker-to-Worker subrequests. Redirects are handled manually and non-success
responses are rejected. Upstream data is bounded to 4096 bytes.

Modes `io-exchange`, `io-delayed`, and `io-slow` exercise real response bodies.
`io-cancel` tests both pre-cancellation and cancellation after headers;
`io-recovery` tests an abandoned generation with I/O in progress. The I/O smoke
also issues distinct HTTP requests and requires matching isolate boot IDs for
the cross-request interruption assertion. These probes prove client-side abort
and draining, not rollback of an upstream side effect or remote destructor execution.
Incoming client-disconnect propagation is not certified by the controlled signal test.

## Disposable CPU-termination proof

The default build excludes the CPU loop. To reproduce the separately authorized
platform fault experiment, build with `WORKERS_G1_CPU_PROBE=1`, deploy using
`termination/wrangler.jsonc`, and install a fresh `PROBE_KEY` with Wrangler's
`secret put`. Keep the matching key in a private local file, outside the repository.
Run `termination/smoke.mjs` with `WORKERS_G1_TERMINATION_URL` and
`WORKERS_G1_KEY_FILE` set. The script verifies unauthenticated access is denied,
triggers the 10 ms platform CPU limit after Kernel Ready, then checks another
healthy request. It requires resource error 1102 and a server-error HTTP status.

Delete the temporary Worker with `wrangler delete --config termination/wrangler.jsonc`,
remove the private key file, and rebuild without `WORKERS_G1_CPU_PROBE` before
updating the regular experiment. The recorded temporary deployment was deleted.
See [I/O and platform evidence](../../docs/evidence/workers-g1/io-and-termination.md).

## Lifecycle and supervision checks

`/probe?mode=lifecycle` runs seven product-neutral fixtures using Kernel 0.3.6,
the existing conformance adapter and Probe contract, and the real Workers Driver.
The regular `smoke` command includes this probe. Cases cover normal shutdown,
prepare/activate rollback, deactivate/release errors, shutdown timeout, and a
finite Plugin restart budget with a stable client across generations.

App readiness remains open during an individual Plugin restart; the initial
activation readiness assertion is restricted to the first generation. Explicit
`report_plugin_failure` tests supervision; it does not establish recoverable
Wasm panic semantics. See [lifecycle evidence](../../docs/evidence/workers-g1/lifecycle.md).
