# Lenso Workers Runtime

This package owns event resources, generation admission and reset, and the
JavaScript timer domain used by the `lenso-workers-driver` Rust crate. It does
not resolve Plugins, grant network authority, authenticate, or authorize requests.
The buffered HTTP Host entry is available starting with version 0.1.2.

Use `lenso-workers-build --manifest Cargo.toml --package my-host --out-dir pkg`
to build a consumer Host with its locked dependency graph. It requires Rust
1.94.0, `wasm32-unknown-unknown`, and wasm-bindgen CLI 0.2.127. `CARGO` and
`WASM_BINDGEN` may select executable paths. No sibling repository layout is
required. The generated module imports `@lenso/workers-runtime/clock`; the Host
must give its runner `clearTimers` from that same module. Use one runner per
generated module/timer domain. Multiple independent Wasm modules require separate
timer domains and are not supported by the shared default clock export.

For a buffered HTTP Host, use the high-level entry with the generated module
namespace and its Wasm module. The scope factory is the explicit composition
seam for Plugin and resource bindings and receives Cloudflare's request, env,
and execution context for every request:

```js
import * as bindings from "./pkg/my_host.js";
import wasmModule from "./pkg/my_host_bg.wasm";
import {
  createEventScope,
  createWorkersHttpHost,
} from "@lenso/workers-runtime";

import { bindStorage } from "./storage.mjs";

const host = createWorkersHttpHost({
  bindings,
  wasmModule,
  limits: {
    eventLimitMs: 1000,
    maxRequestBodyBytes: 64 * 1024,
    maxResponseBodyBytes: 64 * 1024,
  },
  createScope(request, env) {
    // bindStorage is the storage owner's adapter: it tracks I/O in this scope.
    return createEventScope(scope => ({
      storage: bindStorage(env.DB, scope),
    }));
  },
  onReceipt(result, response, request, env, ctx) {
    response.headers.set("x-host-generation", String(result.generation));
  },
});

export default { fetch: host.fetch };
```

The entry wires `initSync({ module: wasmModule })`, reset-state support, Wasm
constructors, and the package timer domain. It returns `{ fetch }`. `limits`
accepts flat runner,
buffered HTTP, and default-scope fields; unknown limit names are rejected.
`onReceipt` also receives the request, env, and execution context. Existing `createEventRunner`,
`createEventScope`, and `createHttpHandler` integrations remain available for
streaming or other Hosts that need lower-level assembly.

```js
import { createEventScope, createEventRunner } from '@lenso/workers-runtime';
import { clearTimers } from '@lenso/workers-runtime/clock';
const runner = createEventRunner({
  instantiate: () => initSync({ module }), resetState: __wbg_reset_state, clearTimers,
});
// Inside this request's owner context, never at module scope:
const scope = createEventScope(scope => ({
  batch: input => scope.run(() => database.batch(prepare(input)), JSON.stringify),
}));
const result = await runner.run(() => invoke(input, scope), { scope, signal: request.signal });
```

An adapter uses `scope.operation(() => ({ promise, abort }))` for abortable work,
`scope.run(start, project)` for unabortable work, and `scope.trackNative(promise)`
for native reads/cancellation spawned by that operation. Native adapters must
bound this work; `trackNative` is not an application admission API. Projection
functions must be JavaScript-only and must not capture Wasm callbacks. The scope
admits at most 128 simultaneous operations by default, rejects new operations on
close, fences stale promise delivery synchronously, and drains native cleanup
including work registered by a late completion. Cleanup has one total 250 ms
budget by default. An uncertain result remains uncertain on repeated settlement;
no retry, rollback, or exactly-once mutation guarantee is implied.

`runner.run` accepts a serialized JSON terminal receipt. `runner.open` accepts
an operation resolving `{ value, closed }`: `value` contains response/session
metadata, while `closed` resolves to `{ shutdown: 'clean' }` only after the body
and App have shut down. It returns `{ value, generation, invoke, cancel, closed }`.
The Host must observe `closed`; failure after headers is a failed stream, not a
replacement HTTP success. Route every subsequent Wasm entry through
`session.invoke(() => ...)`. Do not call old Wasm destructors after abandonment.
`cancel()` signals the event scope; it does not fabricate a terminal receipt.
A cancellation must settle within `cancellationLimitMs` (default 1 s), after
which the generation is abandoned.

Headers keep the original event startup deadline (default 1 s). After opening,
the session has `sessionLimitMs` (default 5 min). Generation retirement waits for
all admitted sessions. A trap or deadline abandons the entire generation,
synchronously fences all admitted scopes, and rejects their owners; native
cleanup runs only in each owner's continuation. These are shared-instance failure
semantics, not independent per-session isolation. WebSocket hibernation is not
provided by this event runtime.

The buffered HTTP compatibility bridge is exported from `./http`. New Host
integrations should create one immutable `createEventScope` for all D1, Fetch and
cancellation bindings. `createCancellationScope` preserves legacy mutable Host
composition only; it is not the preferred API.

Run `npm test` for focused resource, HTTP, and session boundary checks. Actual
Workers deployment and each Plugin's own conformance remain separate evidence.

`createStreamingHttpHandler` consumes an `openHttp` adapter returning
`{ value: { status, headers, read }, closed }`. Each `read()` yields one
`Uint8Array` or `null`; the adapter must finish App shutdown for `closed` before
clean EOF. Reads begin only on consumer demand, copy the current Wasm memory
view, and enforce per-chunk and total byte limits. Disconnect cancels the lease;
a failed generation errors an outstanding body read. The real Rust/Wasm duplex fixture is qualified on the deployed Workers target;
receipts are in `experiments/workers-g2/evidence/duplex.json`. Supply Web's
`createWebSocketTransport()` through `upgradeWebSocket` for authorized status-101
responses. Web owns that transport and its Capability, not Runtime.

When supplying `createScope`, configure its cleanup and operation limits in that
factory. Passing scope limits to the Host at the same time is rejected, so a
custom factory cannot silently ignore a Host limit.
