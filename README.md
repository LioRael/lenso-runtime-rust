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
```
