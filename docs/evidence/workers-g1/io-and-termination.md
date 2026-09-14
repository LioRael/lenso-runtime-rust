# G1 request-owned I/O and platform termination — 2026-09-14

This follow-up implements the next qualification slice after `20af238`. Local
workerd and deployed Workers pass real upstream I/O isolation, cancellation,
generation abandonment, and subsequent network recovery. A separate private
Worker also verified actual Cloudflare CPU-limit termination after Kernel Ready.
G1 as a whole remains experimental; the remaining gates are listed below.

## Host ownership and behavior

The fetch handler creates an I/O scope and passes that exact object into the Rust
Host. While its App is Ready, the Host awaits a real upstream exchange, then
invokes the existing Endpoint binding. The scope is not stored in a global
current-request registry, a serialized Plugin configuration, or Kernel extensions.
This exercises Runtime Host mechanics; it is not a new public network Capability
and does not qualify a Plugin's own external-client implementation.

Each scope owns its AbortController, fetch and bounded response reader. Successful
completion closes it. Cancellation aborts its I/O; generation abandonment aborts
all admitted scopes before resetting Wasm. The failure response waits for its
scope to settle. Pre-cancellation returns an aborted result without issuing fetch
or discarding a concurrent healthy event. No cleanup result implies rollback of
an upstream durable write.

The stateless upstream returns each request's distinct value and encoded identity
header. It supports immediate, 200 ms delayed, and 3000 ms slow streamed bodies.
The main experiment bounds upstream bodies to 4096 bytes. In cancellation tests,
headers must have arrived before cancellation counts as passing. These checks
therefore exercise real response-body I/O rather than only an unstarted Promise.

## Evidence

See [local results](local-io-smoke.json), [remote results](remote-io-smoke.json),
and the [CPU termination observation](platform-termination.json).

- Twelve concurrent HTTP requests with distinct Unicode values retain the correct
  identities. Alternating immediate/delayed bodies exercises overlapping request
  lifetimes, including when shorter requests finish first.
- A pre-cancelled scope performs no fetch and a concurrent healthy exchange succeeds.
- Cancellation after upstream headers aborts the reader, leaves zero pending I/O,
  and permits the App's ordinary clean shutdown.
- An async trap with I/O pending abandons the generation, aborts that I/O, and
  permits a fresh-generation network exchange within the same fetch event.
- Two separate HTTP requests with matching `x-probe-boot` IDs prove a shared
  isolate. One synchronous fault interrupts the other's pending upstream body;
  its result confirms one abort, no completed exchange, and zero pending I/O.
  A later normal network request succeeds.
- Existing runtime smoke and upstream request-conformance subset remain passing.
  Locked release build, Rust formatting, and Clippy with `--all-features`
  and `-D warnings` passed for `wasm32-unknown-unknown`.

Observed platform differences were fixed at the host boundary. The pinned
workerd rejects `redirect: "error"`; the scope uses manual redirects and rejects
non-success responses. Remote Worker-to-Worker fetch initially returned 404 until
`global_fetch_strictly_public` was explicitly enabled. Local-only success did not
count as remote proof.

## CPU-limit experiment

The optional `cpu-probe` Cargo feature adds a continuous-computation fixture
**after Kernel Ready**. The separate `lenso-workers-g1-termination` Worker used
that build with `cpu_ms: 10` and a fresh secret header key. Unauthenticated access
was verified as 404 before the destructive probe. The ordinary experiment's build
excludes this feature and exposes no CPU-loop mode.

The platform returned HTTP 503 containing resource-limit error **1102**. An
initial assertion that assumed HTTP 500 failed; the final probe checks a server
error status plus the specific 1102 code. A subsequent ordinary request completed
cleanly. The forced request has shutdown **unconfirmed**; neither Rust destructors
nor the JS event deadline were claimed to preempt the loop.

The before/after boot IDs differed, but independent HTTP routing prevents treating
that alone as proof of a particular isolate's eviction. The proven fact is actual
CPU-limit failure followed by service recovery. Platform-directed eviction and
memory-limit termination remain separate tests.

The temporary Worker was successfully deleted through Wrangler, and its private
local key file was removed. Its source is retained under `termination/` for an
explicitly authorized reproduction. The stateless upstream remains deployed to
support the regular experiment's I/O smoke.

## Deployed regular artifacts

- Runtime experiment version: `3c5ee04b-5f63-4228-8c7b-736d6e1dea2b`.
- Wasm SHA-256: `3b28822e7ef419e6deca29d379605f62d5c0a79b4f9827ce2601fa0224904705`.
- Total upload: 1484.03 KiB; gzip: 439.30 KiB.
- Upstream version: `4fdd17a8-6a86-4d4c-989e-18e19e329f18`.
- Main endpoint: https://lenso-workers-g1-proof.lenso.workers.dev/probe?mode=io-exchange&input=hello
- Compatibility date: 2026-07-08; public-fetch flag explicitly enabled.
- Initial CPU-fixture code upload: `024065a9-45ea-4dfb-85b8-75c12f6524d1`,
  followed by secret installation; the temporary deployment is now deleted.

No production Marketplace, Auth, or supported Runtime package was changed.
Reproduction commands are in the [experiment README](../../../experiments/workers-g1/README.md).

## Remaining qualification

Complete applicable lifecycle/supervision conformance, remote CPU baselines and
complete peak-memory qualification before package promotion. Controlled AbortSignal
cancellation does not prove actual browser/client-disconnect propagation. The I/O
scope here is a Host probe; real Plugin-owned network/storage implementations must
be qualified separately. Explicit isolate eviction, memory exhaustion, and durable
side-effect recovery are not established by CPU-limit failure or local abort.

## Primary platform references

- [Workers fetch and Worker-to-Worker routing](https://developers.cloudflare.com/workers/runtime-apis/fetch/)
- [Compatibility flags](https://developers.cloudflare.com/workers/configuration/compatibility-flags/)
- [Request cancellation](https://developers.cloudflare.com/workers/runtime-apis/request/)
- [Platform limits](https://developers.cloudflare.com/workers/platform/limits/)
- [Workers errors and request I/O ownership](https://developers.cloudflare.com/workers/observability/errors/)
