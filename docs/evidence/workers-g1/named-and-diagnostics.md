# Workers G1 named dependencies and diagnostics — 2026-09-14

Two named-dependency vectors and seven diagnostic vectors from
`lenso-runtime-conformance` 0.3.2 now run on the real Workers Driver. This closes
those rows in the [coverage inventory](conformance-matrix.md), not the entire G1 gate.

## Source and adaptations

The new `named_dependencies_proof.rs` and `diagnostics_proof.rs` under the
experiment Host retain upstream fixture plans, factories, and assertions.
Deterministic `driver.run` is replaced with awaited execution. Each Driver has
an EventGuard for abandonment cleanup. The named-view tests now additionally
await Clean shutdown after each configuration.

Real time cannot assert a deterministic timestamp of zero. The shutdown vector
waits on a real 10 ms Driver timer, checks exactly one admission closure, cleanup
start and completion, and verifies completion elapsed equals time since cleanup
start. The invocation vector bounds diagnostic duration by its enclosing measured
interval. These are timing adaptations, not unchanged deterministic schedule proof.

Assertions are experimental test assertions, not production error handling. On
failure the Wasm abort is handled by the Runner's existing abandonment/deadline
boundary and the smoke fails; no clean cleanup is claimed for an assertion abort.

## Observed behaviors

- Named source and optional destination views preserve the generated client
  interface with absent, same-provider and alternate-provider destinations.
- Unqualified lookup remains ambiguous when two requirements use one Capability.
- Two named handles share the provider's capacity. A terminal reply with a retained
  execution lease does not admit another request; settling the lease restores
  capacity. Dropping an unsettled lease remains uncertain and shuts down as Timeout.
- Diagnostic source filtering and overflow counters work without blocking shutdown.
- An observer awaiting its next record wakes under the real Driver.
- No observers, observer disconnect, and rejected zero-capacity subscriptions do
  not prevent the empty App from starting and shutting down cleanly.
- Shutdown records actual admission/cleanup boundaries and cleanup-relative duration.
- Unresolved caller text is not promoted into structural identity fields.
- Request diagnostics preserve success, domain-error and unknown-operation categories.
  They retain the upstream typed diagnostic schema, without adding request payloads.

## Validation and deployed artifact

- Locked release Wasm build, all-features target Clippy with warnings denied, and
  Rust formatting checks passed.
- [Local full smoke](local-named-diagnostics-smoke.json) and
  [remote full smoke](remote-named-diagnostics-smoke.json) passed. Both explicitly
  require two named-dependency cases and seven diagnostic cases alongside earlier
  lifecycle, invocation and fault-recovery checks.
- Worker: https://lenso-workers-g1-proof.lenso.workers.dev
- Version: `723323a4-fc5f-49e2-95b0-b1e04cd96347`.
- Wasm SHA-256: `19a9aab8c3f97557b26c1f7bf909cfda66b2a53fdbf055de273d0721acc3f732`.
- Upload: 1744.34 KiB; gzip: 504.95 KiB. Wrangler startup: 1 ms.

The growing artifact includes test fixtures, so its size is not the projected
production Driver overhead. No production package or HTTP contract changed.

## Remaining gate work

Stream/event interactions, remaining Plan/binding vectors and deterministic
interleaving equivalents are still pending in the inventory. The previous
bounded workload does not establish complete remote CPU or total peak-memory
qualification. These remain explicit requirements before supported Runtime
promotion; Auth and HTTP ingress parity are separate later gates.
