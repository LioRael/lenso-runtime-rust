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

## Bounded load and remote metrics

```sh
WORKERS_G1_URL=https://lenso-workers-g1-proof.lenso.workers.dev node load.mjs > /tmp/g1-load.json
# Run separately, with at least two seconds between measurement windows:
WORKERS_G1_URL=https://lenso-workers-g1-proof.lenso.workers.dev WORKERS_G1_LOAD_MODE=io-delayed node load.mjs > /tmp/g1-io-load.json
# After analytics ingestion, using your existing credential environment:
WORKERS_G1_SCRIPT=lenso-workers-g1-proof python3 metrics.py /tmp/g1-load.json
```

The load script defaults to 240 requests at concurrency 12. It caps requests at
2400 and concurrency at 24, and accepts only normal, io-exchange or io-delayed.
Each response must echo the unique input, report one invocation and Clean shutdown.
The receipt keeps individual latency, isolate boot identity, generation and Wasm
capacity samples. This is a closed-loop diagnostic workload, not Marketplace traffic.
Do not run fault probes or other workloads in its measurement window.

The metrics reader requires `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` in
the environment. It performs one read-only official GraphQL query, does not store
credentials, and reports absent/partial metrics with `count_matches: false`.
It expands query boundaries to whole seconds and records those boundaries. Leave
other suites outside that expanded window; wait for ingestion before re-querying.
CPU quantiles retain API-native units and must not be averaged across rows.
Neither script measures total isolate peak memory. See the
[load evidence](../../docs/evidence/workers-g1/load-and-coverage.md) and
[upstream coverage inventory](../../docs/evidence/workers-g1/conformance-matrix.md).

## Named dependency and diagnostics vectors

The regular smoke now includes `/probe?mode=named-dependencies` (two upstream
vectors, including all three optional-destination configurations) and
`/probe?mode=diagnostics` (seven upstream vectors). The fixtures are ported from
`lenso-runtime-conformance` 0.3.2 with real Workers scheduling and explicit cleanup.

Deterministic `run` calls become awaited operations. Shutdown timestamp checks
use a real 10 ms timer, event ordering and cleanup-relative elapsed time;
successful invocation duration must fit within the surrounding measured interval.
A retained execution lease continues to consume provider capacity after a reply;
dropping an unsettled lease still produces Timeout rather than a clean completion.

Fixture assertions abort Wasm on regression; the existing Runner abandonment
boundary rejects the probe. An assertion failure is never a passing conformance
result. See [named dependency and diagnostics evidence](../../docs/evidence/workers-g1/named-and-diagnostics.md).

## G1 acceptance and complete suite

The [G1 acceptance report](../../docs/evidence/workers-g1/acceptance.md) is the
current status; earlier evidence reports remain historical snapshots.
`smoke.mjs` now includes all 32 mapped upstream behavior vectors, plus the
experiment's lifecycle, Driver, fault and recovery checks. The schedules probe
runs 25 cancellation/completion combinations and five deadline/completion cases
using actual host timers. These assert terminal outcomes, not deterministic
wall-clock ordering.

Run `smoke.mjs`, `io-smoke.mjs`, and `disconnect-smoke.mjs` **sequentially**.
Fault probes intentionally abandon a shared Wasm generation and can invalidate
another suite's in-flight events. The disconnect probe requires
`enable_request_signal`, explicitly aborts an HTTP fetch after receiving a
streamed identity, and verifies the same-isolate completion receipt. Receipt
storage is bounded to 16 entries with lazy 60-second expiration. Missing receipts
can reflect routing; they are never treated as a pass. A sanitized remote tail
receipt can additionally establish platform cancellation without relying on
routing. Wrangler's local HTTP proxy did not reliably propagate this cancellation;
this specific boundary is qualified on the deployed Worker, while controlled I/O
cancellation and all conformance vectors also run locally.

The Runner only rejects peer events during generation abandonment. Each event
cancels its native I/O in its own registered Promise continuation; direct foreign
request cancellation can throw in workerd and must not be reintroduced.

`load.mjs` permits `WORKERS_G1_INPUT_SIZE` up to 1024 characters and uses a 30-second
client transport timeout; the Runner still has its independent 1-second deadline.
`metrics.py` now reports the API sample interval, CPU quantiles and maximum, isolate
memory maximum, and Wasm memory maximum. Units are verified through live GraphQL
schema introspection: CPU microseconds and memory bytes. Counts are estimates under
adaptive sampling, so `count_matches` is informational, not a completeness gate.
Memory maxima are over observed invocations, not continuous unsampled process peaks.

## Disposable memory-limit experiment

Deploy `termination/memory.jsonc` and set a fresh private `PROBE_KEY`. With
`WORKERS_G1_TERMINATION_URL` and `WORKERS_G1_KEY_FILE` set, run
`termination/memory-smoke.mjs`. The separate wrapper starts Kernel, then applies
bounded 512 MiB pressure through the Host I/O seam. It does not add a production
mode or require the optional CPU-loop build. Confirm `exceededMemory` in platform
analytics (1102 alone also describes CPU limits), then delete that Worker and
remove the key. The recorded experiment and its key were removed after validation.

## Bounded Wasm generation lifetime

Request App shutdown does not imply linear-memory capacity shrinks. The reference
workload initially showed roughly linear capacity growth inside one generation.
The Runner therefore retires an idle generation after 64 admissions, with a hard
96-admission ceiling while it drains. At most 32 requests wait, each on its own
1 ms timer and a 1-second admission deadline; there is no shared cross-request
promise. At most 32 events execute at once. Successful requests finish I/O cleanup
before retirement. The fresh instance runs the static constructors again; logical
App state remains per request. Failure abandonment remains a separate path.

Run a load of at least 1,000 requests, then:

```sh
node retirement-smoke.mjs /tmp/g1-load.json
```

The check requires repeated memory-capacity reductions within the same isolate,
at most 96 observed requests per generation and an 8 MiB observed Wasm bound for
this reference workload. It does not claim every arbitrary Plugin graph fits
that bound or that the portable Kernel's retained-allocation source is resolved.
