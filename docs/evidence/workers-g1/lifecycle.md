# Workers G1 lifecycle follow-up — 2026-09-14

Status: seven lifecycle/supervision cases pass locally and on the deployed
experimental Worker. G1 remains open; no production Runtime, ingress, Auth, or
Marketplace migration is implied.

## Scope and observations

Source: `experiments/workers-g1/host/src/lifecycle_proof.rs`. Kernel 0.3.6 runs
on the existing experimental Workers Driver, using conformance 0.3.2's adapter,
factory and typed Probe endpoint/client. The provider/consumer fixture plan is
product-neutral. It does not introduce a product resolver or HTTP contract.

| Case | Observed assertion |
| --- | --- |
| Normal | Ready, immediate admission closure, exact AdmissionClosed invocation failure, consumer-before-provider cleanup, each resource released once, repeated shutdown does not repeat cleanup |
| Prepare failure | PluginFailure before Ready; failed consumer and prepared provider roll back in dependency-safe order with StartupRollback reasons |
| Activate failure | Same rollback order and reasons; managed task count is zero on entering deactivate |
| Deactivate failure | Shutdown reports PluginFailure rather than Clean |
| Release failure | Shutdown reports PluginFailure rather than Clean |
| Shutdown timeout | Pending deactivate produces Timeout under a 25 ms shutdown budget |
| Supervision | Provider generation advances to two; the previously created client invokes the replacement; a second failure exhausts the one-restart budget, reports PluginRestartExhausted, and closes admission |

The initial fixture incorrectly required App readiness to be closed during every
activation. Individual Plugin restart leaves App readiness open. Restricting the
assertion to the initial generation corrected the fixture; this was not a Driver
or Kernel bug. Clean cleanup after terminal supervision failure is checked
separately from the terminal failure itself. Supervision uses explicit failure
reporting; Wasm panic recovery remains governed by the generation-abandonment
checks described in the recovery report.

Build failures now preserve rendered Cargo compiler diagnostics before removing
the temporary build receipt. Generated wasm-bindgen files remain unmodified.

## Validation and artifact

- Locked release Wasm build passed.
- Rust formatting check and target Clippy with all features and warnings denied passed.
- [Local full smoke](local-lifecycle-smoke.json) passed.
- [Remote full smoke](remote-lifecycle-smoke.json) passed.
- [Remote I/O regression](remote-lifecycle-io-smoke.json) passed after the full smoke,
  including the shared-isolate cross-request failure assertion.
- Worker: https://lenso-workers-g1-proof.lenso.workers.dev
- Version: `e7a817d7-3a01-43ee-8597-fa084901b4c5`.
- Wasm SHA-256: `e3e7a1c24f8cfbe36d5e97631e4ee4b392a697a286415d21b73554f835bdc4f9`.
- Wrangler upload: 1543.55 KiB; gzip: 456.51 KiB; reported startup: 1 ms.
  Startup is not request CPU or latency.

Reproduce with the experiment's build instructions, then run `smoke.mjs` and
`io-smoke.mjs` sequentially with WORKERS_G1_URL set to the intended endpoint.
Concurrent fault suites can intentionally invalidate each other's generations.

## Remote analytics sample

The existing Cloudflare login successfully read `workersInvocationsAdaptive`
through the official GraphQL API. No additional credentials or permissions were
needed. [Raw sample](remote-lifecycle-metrics.json) records the exact UTC window,
script filter, request/error sums and CPU quantiles in API-native units. Query
shape follows [Cloudflare's Workers metrics tutorial](https://developers.cloudflare.com/analytics/graphql-api/tutorials/querying-workers-metrics/).

The window contains 135 requests across experimental deployments and diagnostic
modes. Its aggregate errors value does not mean every HTTP request succeeded:
intentional faults are caught and returned as diagnostic HTTP errors. This sample
establishes remote metric access only, not a representative CPU baseline or peak
memory measurement. It must not be attributed exclusively to the version above.

## Remaining qualification

Map the remaining applicable upstream conformance cases before claiming complete
conformance. Establish representative remote CPU measurements and peak-memory
bounds, actual client-disconnect behavior, and memory-limit termination semantics.
The prior CPU-limit experiment and fresh-Wasm-generation recovery do not certify
platform-directed isolate eviction. Plugin-owned network/storage, durable side
effects, HTTP ingress parity and Auth composition still need their owning gates.
