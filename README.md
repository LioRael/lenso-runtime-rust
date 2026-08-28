# Lenso Rust Runtime

Rust host-side Runtime Drivers and Execution Adapters for the portable Lenso
Kernel. Implementations are verified across the published
`lenso-runtime-conformance` Interface; this repository does not own Plan or
Kernel semantics.

The source was extracted from `LioRael/lenso` at monorepo commit
`67d21499548d07e92c2f6529d7c8345e58c067d9` under ADR 0064. Imported subtrees
retain their relevant Git history.

`lenso-test` provides a deterministic `TestApp` that boots an immutable Plan
through the real Kernel and native Adapter for Plugin integration tests.

## Embed a Host

Enable the `host` feature on the framework facade when building a product Host:

```toml
lenso = { version = "0.5", features = ["host"] }
```

`lenso::host::HostBuilder` accepts the product's App identity, exact
`GenerationRuntime`, and durable `ControlStateStore`. It opens, recovers, or
replaces a suspended Controller and returns a running Host with fenced routes,
transitions, inspection, and exact suspend/shutdown handshakes. Product code
continues to own App resolution, Plugin policy, Profile semantics, and the
recovery authority supplied to `recover`.

The Controller is lane-local and uses `spawn_local`; start it inside the same
Tokio `LocalSet` that owns the product Host. Kernel semantics remain below this
facade and product loops remain above it.

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
cargo check --locked -p lenso-browser-driver --all-targets --target wasm32-unknown-unknown
cargo check --locked -p lenso-wasip2-driver --all-targets --target wasm32-wasip2
wasm-pack test --headless --chrome crates/lenso-browser-driver --locked
CARGO_TARGET_WASM32_WASIP2_RUNNER=wasmtime cargo test --locked -p lenso-wasip2-driver --target wasm32-wasip2 --test smoke
```

The Browser command requires `wasm-pack`, Chrome, and a matching ChromeDriver.
The WASIp2 command requires Wasmtime on `PATH`.

## Host support policy

- Native Tokio is the supported production host. Its Runner owns replicated
  lane lifecycle, terminal failure propagation, diagnostics, and zero-copy
  transfer for registered generated Request, Stream, and Event values.
- Browser (`wasm32-unknown-unknown`) is a supported Runtime Driver target. CI
  executes its Kernel lifecycle smoke suite in real headless Chrome; native
  replicated lanes are not part of the Browser target contract.
- WASIp2 (`wasm32-wasip2`) is experimental and is not published yet. CI
  executes its Kernel lifecycle smoke suite as a component in Wasmtime.
- Native fallback implementations exist to keep host-independent development
  tests fast. Passing a fallback test is not accepted as target-host evidence.

## Byte-oriented guest ABI

The preview Wasm Component and QuickJS Adapters preserve the stable
`lenso.json-request@1` ABI and additionally implement
`lenso.json-interactions@1` for Request plus bidirectional Stream Capabilities.
Guests with declared requirements use `lenso.json-host-imports@1`. During
activation the Host exposes an opaque binding table derived only from the
immutable Plan; there is no ambient Capability lookup or mutable registry.
Generated Capability codecs translate portable JSON into typed dependency
handles, so imported calls retain Kernel admission, deadlines, cancellation,
supervision, and diagnostics. QuickJS exposes hardened synchronous import
functions and Wasm uses explicit Component Model imports; both support Request
and bidirectional Stream calls with bounded per-call and live-session limits.
Before opening readiness, a guest must expose
an exact `describe` result containing every provided and required Capability,
Descriptor version, cardinality, interaction kind, and Operation selected by
the immutable Plan.
Generated codecs lower typed open requests, messages, and Domain Errors to
portable JSON. Stream guests expose explicit open, send, receive, half-close,
and cancel functions; the Host bounds both worker admission and live sessions.
Rust guests use `lenso-guest-sdk` to load the activated binding table once and
consume generated Capability clients. The SDK keeps Domain Errors, bounded
Runtime Failures, and guest protocol failures distinct, and cancels live Host
Streams when their typed guest handles are dropped. Adapter-specific
`wit-bindgen` imports are connected through the `wasm_host!` macro; application
code does not parse envelopes or manage opaque stream identities.
Descriptor drift, duplicate host codecs, unsupported Event Operations, missing
entrypoints, and unadmitted Artifacts fail before Plugin activation.

Event support remains fail-closed. The Adapters do not emulate Stream or Event
semantics through unbounded arrays, callbacks, or hidden runtime negotiation.

## Trusted Process Plugins

`lenso-process-adapter` executes one precompiled child process per Plugin
Instance through bounded framed stdio. `lenso-process-sdk` owns the guest wire,
so business code implements typed handlers rather than protocol messages. The
first execution class, `lenso.process@1`, supports request providers only and
rejects Streams and Host imports before readiness. Cancellation retires the
process and never replays a request.

Process Plugins are trusted code, not a hostile-code sandbox. The Adapter
starts with an empty environment and no inherited secret configuration, but a
native executable retains ambient operating-system authority until a reviewed
platform sandbox enforces narrower grants. Use the Wasm Component Adapter for
untrusted third-party code.

## Remote Plugins

`lenso-remote-adapter` runs request-only Plugin providers behind an HTTP
deployment while preserving Lenso's Plan and Generation boundaries. The Host
admits a small digest-verified deployment-binding Artifact for each Instance,
performs an exact descriptor handshake, bounds concurrency, response size, and
request size and timeouts, and propagates cancellation without retrying an
invocation. A binding uses the following shape:

```json
{
  "schema_version": 1,
  "protocol": "lenso.remote-http-json@1",
  "base_url": "https://plugin.example.com/"
}
```

The service implements `GET /lenso/v1/ready`, `POST /lenso/v1/invoke`, and
`POST /lenso/v1/cancel`. HTTPS is mandatory outside loopback development.
Authentication and streaming are intentionally not part of Remote V1; products
can inject a product-owned HTTP client for proxy, default headers, or mTLS and
should terminate authorization policy in their deployment layer.

## Portable Plugin authoring

`lenso-plugin-sdk` is the domain-neutral lowering layer for portable Rust
Plugins. Product SDKs own typed Capability authoring (for example Agent Tools)
and generate one hidden JSON request dispatcher. This Runtime SDK lowers that
dispatcher to a Wasm Component when Cargo targets `wasm32`, and to the framed
Process protocol for native binaries. It does not contain Agent, Ingress, Auth,
or other product semantics. WIT bindings, runtime descriptors, and stdio framing
remain SDK-owned implementation details rather than files in each Plugin project.

## Releases

Published crates use release PRs and crates.io Trusted Publishing through
`.github/workflows/release-plz.yml`. Each published package must allow the
`LioRael/lenso-runtime-rust` repository and that exact workflow basename in its
crates.io Trusted Publisher settings. The workflow requests GitHub OIDC only at
release time and does not use a long-lived Cargo registry token.
