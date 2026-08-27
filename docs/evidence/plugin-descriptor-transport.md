# Plugin descriptor transport evidence

## Chosen transport

The first Rust/Wasm Plugin shape emits canonical JSON into the
`lenso.plugin-descriptor.v1` WebAssembly custom section. The
`guest_request_plugin!` declaration is the only Capability/operation input: it
generates both that section and the runtime `describe()` response from the same
compile-time string.

`wit_component::ComponentEncoder` preserves the section while converting the
Rust core Wasm module into the exact final Component. Packaging reads the
section from those final bytes, then computes the artifact digest and size.
Consequently, a descriptor from one build cannot be paired with another
Component without failing artifact or descriptor verification.

Focused tests establish that:

- a real Rust guest retains byte-identical descriptor evidence after
  componentization;
- the runtime description and packaging section share one canonical encoder;
- missing, duplicate, malformed, non-canonical, and descriptors larger than
  64 KiB fail closed;
- changing descriptor bytes changes the final artifact digest;
- schema V2 rejects conflicting publisher fields and verifies the descriptor
  again from the packaged Component;
- the builder reopens and verifies the final output directory after writing it.

## No publisher code executed

The builder parses static Wasm sections with `wasmparser`. It never constructs
a Wasm engine, instantiates the Component, links imports, or calls `describe()`.
The integration proof builds and packages a guest whose runtime code is never
started during packaging.

## Rejected

- An adjacent descriptor was rejected because it introduces a second movable
  file whose relationship to the executable must be recreated and secured.
- A fixed Harness-only profile was rejected because the preserved custom
  section provides a general single-authority path without narrowing the first
  Plugin to more hard-coded host behavior.
- Calling runtime `describe()` during packaging was rejected because it would
  execute untrusted publisher code and create packaging-time side effects.

## Commands and measurements

The proof uses the real `rust-guest` fixture and runs through
`wasm32-unknown-unknown` release compilation followed by Component encoding.
The descriptor limit is 65,536 bytes. Verification commands are the focused
`lenso-guest-sdk`, `lenso-plugin-bundle`, and
`lenso-wasm-component-adapter` tests plus the repository-wide Runtime matrix.

## Exact limitations

This slice supports one request-style entry in one Rust-authored Wasm Plugin.
It does not cover multiple entries, stream/event declarations, QuickJS,
processes, native dynamic libraries, data-only Plugins, permissions, state
migration, publisher Features, or binding templates. Runtime lowering into
existing Module records remains private to the control plane; Kernel and
Resolved App Plan bytes are unchanged.
