# W02 local qualification

Status: **passed locally**. Original design base: `9fbed717aa1a5f7a31d56cf993877be58cdc3d0f`; integrated and revalidated on W06 base `76587054ccc65a6f9510d1acd13493cfab3f4c0a`.

## Executed evidence

- [Runtime/build validation](validation.json): 117 Workers runtime tests pass after W06 integration;
  the new deterministic quarantine regression fails against the preserved base
  Runner because successor body preparation starts while owner cleanup is pending.
  Locked wasm32 check, locked release build and Rust formatting pass.
- [Actual workerd test receipt](workerd-test.json.gz): 11 cases pass on Rust 1.94.0,
  wasm-bindgen 0.2.127 and workerd 2026-07-01. The HTTP/session scripts have
  distinct boot identities, one generated bindings/clock domain each, and the
  same exact Wasm artifact hash. Module inventories and source/lock hashes are
  included. This is a supplementary service-binding harness, **not external
  socket or deployed routing evidence**.
- The mixed control reaches 96 cumulative admissions with one session retained,
  rejects the next admission at its existing deadline, and cleans up on release.
  Each split held-stream/held-WebSocket case completes 192 short HTTP requests
  across three HTTP generations in the same boot. Total successful short requests
  including health probes: 395. Receipts are collected outside the tested Worker;
  in-isolate lookup remains bounded to 16 records with lazy 60-second expiry.
- Capacity holds 32 real Rust sessions and rejects the 33rd. Cooperative and
  repeated owner cancellation preserves peers. Traps, rejected Wasm exports and
  injected non-clean terminal receipts fence old leases/classes and change memory
  identity. During cleanup quarantine the unrelated HTTP script serves a request.
  Late native resolve/reject allows recovery without stale delivery or changing
  the uncertain receipt. Rejected custom cleanup deliberately remains unavailable.
- [External local matrix receipt](external-local.json.gz) passed all 11 required cases through three real local Wrangler/workerd listeners. The HTTP and session Workers retain distinct boot identities and route ownership; the mixed Worker is a baseline control only. [Environment evidence](environment.json) distinguishes the earlier sandbox listener failure from the coordinator pass. Raw receipts are gzip-compressed; use `gzip -dc` to inspect them without expanding repository history.

The Runner resets generated guards synchronously on abandonment, then admits no
replacement event until the bounded owner batch confirms release. A timed-out
built-in scope separately observes late release without extending its response
cleanup deadline. Failed abort/custom cleanup and reset/constructor failures stay
closed. Late headers no longer restart the first cancellation watchdog.

## Exact boundaries

The required external HTTP/WebSocket matrix passed on a coordinator with loopback access. It covers route rejection before admission, the mixed baseline, held streams and WebSockets, capacity, cancellation, cleanup quarantine, late native release/rejection, rejected Wasm exports, non-clean session receipts and rejected cleanup. The supplementary service-binding run independently passed the same 11 cases.

The existing G1/G2 network suites were not rerun as a separate cohort; the W02 matrix directly exercises the split profiles and lifecycle cases required by this change. The held stream uses a controlled owner-side release signal and does not claim downstream disconnect propagation. WebSocket closure uses local workerd transport.

No public contract, Kernel semantics, admission ceiling or deadline was raised.
Published Web Ingress 0.4.3 is pinned because published 0.4.2 pulls unsupported Mio
networking into wasm32. Vendored fixtures and JS transport record immutable Web
source provenance; they do not require unpublished crates or sibling worktrees.
The required Cargo wrapper ran with a temporary target root and identity compiler
wrapper because the default shared cache/sccache are denied by this sandbox.

There is **no Cloudflare production, external IdP, product/fleet capacity,
physical isolation, throughput, latency, memory peak or cost claim**. No deployment or production traffic was used. Generated build, bundle and install artifacts and task-owned processes are removed after validation.

Reproduction commands and case selectors are in the
[G2 README](../../../experiments/workers-g2/README.md#w02-local-qualification).
