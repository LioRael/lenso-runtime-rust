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
