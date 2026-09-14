# G1 acceptance — Cloudflare Workers generic Runtime

Date: 2026-09-14. **G1 is qualified for the bounded reference profile below.**
This is a generic Runtime feasibility result, allowing work on G2 HTTP parity.
It does not certify arbitrary Plugin graphs, production Marketplace, or Auth.

## Accepted profile

Pinned Kernel 0.3.6, App Plan 0.4.1, conformance 0.3.2, HTTP Endpoint 0.3.1,
Rust 1.94.0, wasm-bindgen 0.2.127 and Wrangler 4.107.0. Compatibility date
2026-07-08, with `global_fetch_strictly_public` and `enable_request_signal`.
Two generated linked Rust Plugins use the existing resolver, Native Adapter and
HTTP Capability. App state belongs to a request; the Wasm instance is shared
within an isolate. Explicit generated linkage anchors remain required.

- Maximum 32 admitted events and 128 Driver tasks per request App.
- Independent 1-second event watchdog; overload and expiry fail closed.
- Idle retirement after 64 admissions; hard generation ceiling 96 admissions.
- At most 32 retirement waiters, each with its own timer and 1-second deadline.
- Reference input up to 1024 characters; upstream response bounded to 4096 bytes.
- Client load timeout is 30 seconds and is separate from the event watchdog.

## Gate evidence

| Concern | Result |
| --- | --- |
| Registration / construction / Ready | Two macro-generated Plugins resolve and invoke the same existing HTTP Capability |
| Upstream behavior corpus | All 32 rows in the [coverage inventory](conformance-matrix.md) have passing real-host fixture coverage |
| Lifecycle and supervision | Rollback order, task/resource cleanup, timeout/failure classification, stable handles and restart exhaustion pass |
| Stream / event | Messages, half-close, terminal/domain errors, fan-out and post-shutdown rejection pass |
| Cancellation races | 25 cancellation/completion combinations and five real-timer deadline/completion cases preserve terminal outcomes |
| Request-owned I/O | [Local](final-local-io.json) and [remote](final-remote-io.json) identity, cancellation and cross-request fault recovery pass |
| Actual incoming cancellation | [Deployed receipt](remote-disconnect.json) shows Request.signal aborted, one upstream abort, no pending I/O and Clean shutdown |
| Fault boundaries | Async/synchronous faults reject admitted events, replace Wasm memory and block stale callbacks; forced shutdown stays unconfirmed |
| Platform limits | Prior [CPU limit proof](platform-termination.json) and new [memory limit proof](memory-termination.json) return platform 1102 and later healthy responses |
| Resource lifetime | [Local concurrency 12](retirement-local.json), [local concurrency 24](retirement-local-24.json), [remote normal](retirement-remote-normal.json) and [remote I/O](retirement-remote-io.json) show repeated same-isolate capacity reductions |
| Native regression | [53 passed](native-regression.json), five existing replicated-lane/routing benchmarks ignored |

The final [local](final-local-smoke.json) and [remote](final-remote-smoke.json)
smokes include all mapped vectors plus Driver, lifecycle and failure checks.
Locked release build, target all-features Clippy with warnings denied, formatting,
JavaScript syntax and Python compile checks passed. The conformance fixtures adapt
deterministic scheduling to real timers; they do not promise deterministic wall time.

## Defects found and addressed

Cross-request abandonment previously called another event's native I/O canceler
synchronously. workerd rejected that with a RefcountedCanceler ownership error,
interrupting recovery. The Runner now rejects peer promises and performs cleanup
in each owner's registered continuation. The paired local upstream and deployed
I/O regression verify the corrected boundary.

[Before retirement](pre-retirement-growth.json), completed requests showed growing
Wasm capacity within a single generation. Clean App shutdown did not establish a
shrinking linear memory. Bounded generation retirement now drains admitted work,
finishes owner-context I/O cleanup, replaces memory, reruns constructors and admits
waiting requests. No successful request is abandoned to reclaim capacity. The
underlying retained-allocation source is not claimed to be fixed or leak-free.

A shared rotation Promise also triggered workerd's cross-request/hung-request
protection. Bounded waiters now use their own request-owned timers. Failure
abandonment and orderly retirement remain distinct outcomes.

## Final deployed workload measurements

Both workloads use 1,200 unique 1024-character inputs at concurrency 12. Every
client response passed input identity, one-invocation and Clean-shutdown checks.
No fault suite ran in these measurement windows. The delayed I/O case includes
a controlled 200 ms upstream delay. Each retirement validator requires at least
two same-isolate capacity reductions, no more than 96 observed requests in one
generation, and an 8 MiB observed Wasm ceiling for this reference workload.

| Workload | CPU p50 / p99 ms | Maximum reported CPU ms | Maximum reported isolate memory MiB | Client-observed Wasm capacity MiB | Same-isolate reductions |
| --- | --- | --- | --- | --- | --- |
| [normal](final-load-normal.json) | 1.445 / 10.498 | 49.136 | 17.40 | 1.94 | 16 |
| [io](final-load-io.json) | 2.522 / 8.775 | 33.260 | 20.97 | 2.25 | 14 |

Raw [normal](final-normal-metrics.json) and [I/O](final-io-metrics.json) analytics
include query windows, sample intervals and estimated request counts. API CPU
units and memory fields were verified against the [live schema](resource-schema.json).
Cloudflare's [memory metrics documentation](https://developers.cloudflare.com/changelog/post/2026-06-30-memory-usage-metrics/)
defines invocation observations. These maxima are **sampled observed peaks**, not
continuous unsampled process high-water marks. Adaptive counts need not equal the
1,200 client requests; equality is not used as a qualification gate. These short
reference runs establish feasibility and bounded generation behavior, not a
production SLA or a bound for every Plugin graph. Local inspector observations
remain [separate](final-local-profile.json).

## Platform termination and diagnostic limitations

The new memory experiment reached `exceededMemory`, confirmed by
[platform status](memory-termination-status.json), with maximum reported CPU
166.213 ms under a 1,000 ms CPU limit. Thus 1102 is not inferred to be a memory
failure solely from its HTTP status. The disposable Worker and private key were
deleted after proof. A following healthy response is verified; changing boot IDs
across HTTP requests alone does not establish which particular V8 isolate was
selected or evicted. Same-fetch Wasm replacement is separately verified.

The incoming disconnect test passes on the deployed Worker with the documented
[Request.signal flag](https://developers.cloudflare.com/changelog/post/2025-05-22-handle-request-cancellation/).
The pinned local Wrangler proxy did not reliably propagate the same disconnect;
its failure is recorded and is not called a local pass. Local controlled
cancellation and the full conformance corpus do pass. Receipt routing can select
a different isolate; missing receipts fail the diagnostic rather than silently pass.

Other interrupted attempts, including a 10-second client network timeout and a
local run using a slow remote upstream, are preserved in
[known test limitations](known-test-limitations.json). They are excluded from
passing load evidence, not erased or counted as successes.

## Artifact and next boundary

- Worker: https://lenso-workers-g1-proof.lenso.workers.dev
- Version: `60cb18f5-9b3b-4424-b6f4-9fa53cd9f7d6`.
- Wasm SHA-256: `59155373d88f66223945d5181e0bc4a08d127e9d390f6da15d8af39e5e57084e`.
- Upload: 2047.06 KiB; gzip: 592.93 KiB; Wrangler startup: 1 ms.
- This artifact includes conformance fixtures; its size is not standalone Driver overhead.

G2 owns shared native/Workers HTTP normalization and ingress parity. Supported
package extraction must preserve the request ownership, retirement budget and
truthful failure boundaries established here. G3 Marketplace reads, G4 Auth
composition and G5 production rollout remain separate gates.
