# G1 async fault recovery follow-up — 2026-09-14

The initial prototype had a real failure-boundary defect: an async Endpoint
`unreachable` left its exported Promise unsettled. Local workerd returned 500
because it detected a hung request, rather than the Runner reporting the failure.
The same issue affected a trap inside a spawned Driver task. The previous
synchronous trap check was insufficient.

## Implemented boundary

The JS Runner now owns a 1000 ms event deadline and a 32-event admission bound.
An uncaught export rejection, synchronous trap, or deadline expiry abandons the
entire Wasm generation. All its pending events reject with `instance_abandoned`
and shutdown `unconfirmed`; host timers are cleared; generated wasm-bindgen reset
support creates a fresh instance. A different memory object is required before
admission resumes. Failed reset leaves admission closed.

This is a Runner facility. It does not modify Kernel, introduce a new execution
class, rebind capabilities, or pretend that a trap is a clean Plugin shutdown.
It also does not reverse durable side effects. Other in-flight requests in the
same generation fail too. The event timer cannot preempt CPU-bound execution.

`--experimental-reset-state-function` generates reset/instance identity guards.
No generated JS is patched. The previous duplicate import-key warning is gone
because the clock now uses one host import with explicit timer ownership.

The recovery probe runs an async trap and a pending invocation together, checks
that both rejected the same generation, then completes a healthy invocation in
the **same fetch event**. This rules out alternate-isolate routing as the reason
for the successful recovery. It proves Wasm recreation, not V8 isolate eviction.

## Validation

Rust 1.94.0 / wasm-bindgen 0.2.127 / Wrangler 4.107.0 remain pinned. Locked release
build, formatting, and Clippy `--workspace --no-deps -- -D warnings` passed.
Both [local](local-recovery-smoke.json) and [remote](remote-recovery-smoke.json)
smoke runs passed, including:

- Existing lifecycle, 12-request concurrency, deadline/cancellation, and shutdown.
- Async Endpoint trap, spawned Driver task trap, and async Plugin panic.
- Generation-wide rejection and same-fetch instance recreation.
- Selected upstream `lenso-runtime-conformance` 0.3.2 request vectors: activation
  dependency, typed response, domain error, unknown operation, provider replacement,
  and clean shutdown. Both default and alternate Provider use the real Driver.

The upstream conformance fixture uses its own product-neutral Adapter and
composition. The two macro-authored Plugins separately exercise the existing
Native Adapter. This is a conformance subset, not the entire upstream suite.

## Remote artifact

- Worker: `lenso-workers-g1-proof`
- Version: `ab60c4ac-1b67-49ef-89d1-607d8e1ee611`
- Wasm SHA-256: `b172fb996b6e931cc34555448ef8dae96a8cdc91e65c48cf6e0fca1a3ff0432e`
- Upload: 1465.35 KiB; gzip: 433.21 KiB (includes diagnostic conformance fixtures).
- Wrangler-reported startup: 1 ms.
- Endpoint: https://lenso-workers-g1-proof.lenso.workers.dev/probe?input=hello

Only the experimental Worker was updated. Marketplace production was unchanged.
The deliberate trap cases emit error logs even when the Runner bounds the response.

## Resource evidence and limits

`node experiments/workers-g1/profile.mjs` records a local workerd V8 CPU profile
and heap observations through its inspector. The [recorded sample](local-profile.json)
verified 240 requests at concurrency 12 with clean shutdown and one invocation
per App. At 1 ms profiler sampling, it recorded 286,447 microseconds in non-idle
samples during a 306,011-microsecond profile window. Non-idle samples include
runtime/profiler overhead; they are not per-request billed CPU.

Twenty observations at batch boundaries saw a maximum used V8 heap of 3,232,692
bytes. This excludes unsampled peaks and does not establish the process or Wasm
memory peak. First/last heap growth alone neither proves nor disproves a leak.
The raw CPU profile is reproducibly emitted to `/tmp/workers-g1-profile.cpuprofile`.

## Qualification still open

G1 remains experimental. No supported Runtime package or G2 migration is claimed.
The remaining work is full applicable lifecycle/supervision conformance,
request-owned I/O under concurrent generation failure, forced platform termination
and isolate recreation, and remote CPU / complete peak-memory qualification.
This follow-up closes the reproduced hanging-Promise fault for the exercised
cases; it does not close those independent gates.

The next implementation should establish request-owned I/O cleanup and failure
under actual platform termination before promoting the Driver/Runner into public
packages. Remote resource baselines must be measured with the selected production
host facilities; diagnostic elapsed time and Wasm memory capacity are not substitutes.

## Primary references consulted

- [Cloudflare Rust panic recovery](https://developers.cloudflare.com/changelog/post/2025-09-19-workers-rs-panic-recovery/)
- [Workers performance and timers](https://developers.cloudflare.com/workers/runtime-apis/performance/)
- [Workers CPU profiling](https://developers.cloudflare.com/workers/observability/dev-tools/cpu-usage/)
- [Workers memory profiling](https://developers.cloudflare.com/workers/observability/dev-tools/memory-usage/)

Implementation decisions were checked against the installed wasm-bindgen 0.2.127
reset generator and js-sys 0.3.104 future-to-promise source, not assumed from the
workers-rs release alone. Lenso does not currently use the workers-rs panic handler.
