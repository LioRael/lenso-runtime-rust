# Workers G2 HTTP proof

This isolated experiment exercises the existing Web Ingress event registration,
its generated HTTP Endpoint contract, and the shared native/event parity fixture
inside a real Workers Runtime Driver and Kernel App. Each HTTP event creates an
App, waits for Ready, enters Web Ingress, and awaits clean shutdown. It is a
disposable staging proof, not production Marketplace or Auth qualification.

The local review cohort is `lenso-web/feat-workers-http-parity` beside this Runtime
worktree. Cargo and the JS corpus import deliberately point to that worktree;
replace them with a reviewed immutable/published cohort for distribution. No
contract bindings are hand-edited and no Endpoint is called outside its Plan.
The shared bridge and generation boundary are documented in
[`workers-runtime`](../workers-runtime/README.md).

Use Rust 1.94.0, wasm-bindgen CLI 0.2.127, and the locked Wrangler 4.107.0/workerd
cohort. The compatibility date and flags match G1. From this directory:

```sh
pnpm install --frozen-lockfile --ignore-scripts
CARGO=/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo bash build.sh
pnpm exec wrangler dev --local --port 63737 --inspector-port 9237
```

In another terminal, run the suites sequentially:

```sh
WORKERS_G2_URL=http://127.0.0.1:63737 node smoke.mjs
WORKERS_G2_URL=http://127.0.0.1:63737 node limits-smoke.mjs
```

For the authorized disposable deployment:

```sh
pnpm exec wrangler deploy --dry-run
pnpm exec wrangler deploy
WORKERS_G2_URL=https://lenso-workers-g2-proof.lenso.workers.dev node smoke.mjs
WORKERS_G2_URL=https://lenso-workers-g2-proof.lenso.workers.dev node limits-smoke.mjs
WORKERS_G2_URL=https://lenso-workers-g2-proof.lenso.workers.dev node disconnect-smoke.mjs
```

The default fetch path enters the actual Request/Response bridge. `/_g2/corpus`
constructs each of the same fixture Requests inside Workers and enters that same
bridge; it isolates Fetch/ingress behavior from edge HTTP rejection. The smoke also
sends every vector over real HTTP, recording responses that never carry a Host
receipt separately instead of counting them as parity. `transport.mjs` uses raw
HTTP so repeated request field lines and TRACE can be attempted, with a finite
transport deadline and fully collected responses.

`/_g2/cancellation` cancels a blocked Endpoint while a peer remains healthy.
`/_g2/recovery` traps the Wasm instance while an HTTP cancellation closure exists,
then requires a healthy fresh generation. `/_g2/body-timeout` supplies a genuine
slow ReadableStream Request to the bridge. `/_g2/response-limit` sends a normal
Plan-bound byte echo through a deliberately smaller four-byte response boundary.
These are diagnostic Host probes, not new business endpoints.

The proof limits request/response bodies to 64 KiB, request heads to 16 KiB, body
reads to 250 ms, Endpoint work to 500 ms, App shutdown to 200 ms, and Runner events
to 1,000 ms. Early rejected bodies are cancelled so native HTTP resources cannot
hold subsequent traffic. These values qualify this fixture only; they are not a
production resource budget or a full-isolate memory measurement.

The disconnect probe writes a padded identity stream with `no-transform` and
identity encoding before the client aborts. It requires a same-isolate receipt
showing the incoming Request signal actually aborted, the Kernel invocation was
cancelled and App shutdown was clean. A response-write failure alone does not pass.
Receipts hold only bounded diagnostic data: 16 entries and a lazy 60-second expiry.
Do not run another fault suite concurrently against the same proof Worker.

Measured results and limitations are in
[the G2 evidence report](../../docs/evidence/workers-g2/README.md).

## W06 request body encoding qualification

The proof imports the HTTP transports and W01 Runner from this worktree. Its
default encoding remains `numeric-array`. To exercise `base64-v1`, start each
existing buffered/duplex Worker with the build-time option
`--define 'G2_REQUEST_BODY_ENCODING:"base64-v1"'`. This is not a request header
or public route option. Run the existing smoke, limits and duplex suites
sequentially for each encoding; preserve the documented disconnect limitations.

After restoring the pinned G2 dependency cohort and completing a fresh build,
also run from the repository root:

```sh
CARGO=/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo \
  node packages/workers-runtime/build.mjs \
  --manifest experiments/workers-g2/Cargo.toml \
  --package lenso-workers-g2-host --out-dir experiments/workers-g2/pkg
node --test experiments/workers-g2/request-body-wasm.test.mjs
```

This additional suite loads the actual generated G2 Rust/Wasm module in Node,
enters the existing Plan-bound byte echo and duplex fixtures through the current
Runner, tests both encodings, holds a real session during overload, and injects
malformed envelopes at both Rust entry points. It requires artifacts and fails
if they are missing. It complements the existing workerd transport suites;
Node cannot prove Cloudflare disconnect behavior. The duplex fixture has only
GET `/stream`, so nonempty POST bodies exercise its existing 405 contract; exact
byte echo uses the buffered fixture and the shared decoder tests.

The [W06 validation record](../../docs/evidence/workers-g2/request-body-encoding.md)
lists unresolved build/check prerequisites. No fresh G2 pass is claimed yet.
