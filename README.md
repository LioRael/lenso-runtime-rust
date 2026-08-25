# Lenso Rust Runtime

Rust host-side Runtime Drivers and Execution Adapters for the portable Lenso
Kernel. Implementations are verified across the published
`lenso-runtime-conformance` Interface; this repository does not own Plan or
Kernel semantics.

The source was extracted from `LioRael/lenso` at monorepo commit
`67d21499548d07e92c2f6529d7c8345e58c067d9` under ADR 0064. Imported subtrees
retain their relevant Git history.

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

The preview Wasm Component and QuickJS Adapters implement the same
`lenso.json-request@1` guest ABI. Before opening readiness, a guest must expose
an exact `describe` result containing every provided Capability, Descriptor
version, and Request Operation selected by the immutable Plan. Invocation then
receives `capability_id`, `operation`, and validated request JSON; the result is
one JSON success or Domain Error value. Descriptor drift, duplicate host codecs,
unsupported Stream or Event Operations, missing entrypoints, and unadmitted
Artifacts fail before Module activation.

This ABI is request-only. Stream and Event support require reviewed generated
codec and guest-lifecycle contracts; the Adapters do not emulate them through
unbounded arrays, callbacks, or hidden runtime negotiation.

## Releases

Published crates use release PRs and crates.io Trusted Publishing through
`.github/workflows/release-plz.yml`. Each published package must allow the
`LioRael/lenso-runtime-rust` repository and that exact workflow basename in its
crates.io Trusted Publisher settings. The workflow requests GitHub OIDC only at
release time and does not use a long-lived Cargo registry token.
