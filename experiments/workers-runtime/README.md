# Shared Workers experiment boundary

These product-neutral JavaScript modules are shared by the qualified G1 experiment
and the G2 HTTP integration. They are review-cohort infrastructure, not a published
Runtime package. The G1 Rust `WorkersDriver` remains the single Driver source;
other hosts include `workers-g1/host/src/driver.rs` without copying its scheduling
semantics. G1's deployed Worker is not changed by local G2 validation.

`createEventRunner({ instantiate, resetState, clearTimers, eventLimitMs,
maxConcurrent, retirementAdmissionLimit })` returns `run(operation, { scope,
signal })` and `generation()`. `operation()` returns the generated Wasm Promise
of serialized JSON. The wrapper initializes static constructors, preserves the
existing generation fences, and appends generation and linear-memory capacity to
the decoded result. Defaults remain 1,000 ms, 32 concurrent events and retirement
after 64 admissions. A consumer may lower concurrency to 1 and choose retirement
from 1 through 96 admissions. Invalid limits fail at startup. The hard admission
ceiling stays 96, with at most 32 bounded admission waiters.

`createHttpHandler({ run, handleHttp, createScope, onReceipt, ...limits })` adapts
an actual Workers Request and returns an actual Response. `handleHttp` is a
generated Wasm export receiving `(serializedRequest, scope)`. Its input shape is:

```json
{"method":"GET","uri":"/items?q=one&q=two","headers":[["accept","application/json"]],"body":[]}
```

The host's result shape is:

```json
{"status":200,"headers":[["content-type","application/json"]],"body":[123,125],"ready":true,"shutdown":"clean"}
```

Large binary responses may replace `body` with `body_base64`, a canonical padded
standard Base64 string. The bridge rejects simultaneous encodings or malformed
bytes with `502 invalid_response_body`, and checks decoded size before allocating
the binary output. Base64 avoids the much larger JSON number-array representation;
it does not eliminate Wasm and JavaScript copies or qualify a memory budget.

The host must dispatch through its Plan-bound Web Ingress Plugin and await App
shutdown before returning. It must cap response bytes before JSON serialization;
the JavaScript bridge also checks the decoded response. Byte arrays amplify JSON
and Wasm memory use, so G1's small-fixture concurrency is not a memory qualification
for larger Marketplace envelopes. A caller supplies its own verified body limits,
head limit and body-read timeout; JavaScript defaults are 1 MiB, 16 KiB and 30 s.
The bridge retains the platform's raw origin-form path/query, including trailing
`?`, and appends response headers so `Set-Cookie` remains separate. Fetch may
already have normalized URL or request headers before this seam.

`createScope(request)` runs inside the failure boundary. A missing required
binding returns 503 before Kernel admission. A custom scope can hold configuration,
bindings and storage callbacks as well as these lifecycle methods:

- `attach(callback)` / `detach()` manage the current Rust cancellation closure.
- `invalidate()` only clears JavaScript references to the old Wasm callback. It
  must never call Rust or another request's native I/O. Generation abandonment
  invokes it before replacing Wasm. A storage adapter must also stop forwarding
  native Promise fulfillment/rejection to old Wasm callbacks after invalidation;
  the minimal cancellation scope does not provide storage Promise fencing.
- `abort()` cancels native work in the owning event continuation. It must be
  idempotent; cleanup can call it more than once.
- `settled()` must finish within the storage adapter's own bound. `false` means
  cleanup is unconfirmed; `true` or `undefined` accepts completion. A false result
  or thrown settlement error produces `503 storage_cleanup_unconfirmed`. The Runner
  also rejects raw callers on this result and invalidates the scope before every
  event removal or instance retirement.

`createCancellationScope(extraFields)` supplies the minimal HTTP implementation.
The combined `cancellation(scope, callback)` import attaches a Rust closure, or
removes it when passed `null`. This single raw import avoids wasm-bindgen 0.2.127's
multiple-key output for separate imports from the same raw module. The Rust guard
example is in `workers-g2/host/src/lib.rs`. Never hand-edit generated glue.

The Runner does not cancel a remote database mutation or retry writes. Adapter
settlement uncertainty must remain visible. A generation failure can invalidate
all simultaneous events in that Wasm instance; only an observed clean App shutdown
qualifies normal completion.

Run the focused JavaScript boundary regressions with:

```sh
node --test experiments/workers-runtime/http.test.mjs
```
