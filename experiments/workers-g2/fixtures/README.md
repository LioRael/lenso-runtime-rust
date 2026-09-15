# G2 qualification fixture provenance

Owner: [LioRael/lenso-web](https://github.com/LioRael/lenso-web), commit
[`6611fd3af560d246e18d6e20d6963f68f1f07566`](https://github.com/LioRael/lenso-web/tree/6611fd3af560d246e18d6e20d6963f68f1f07566).
The inspected owner checkout was clean. Source was copied with `git show` at
this commit, never from uncommitted files. The owner declares the MIT license.

The two `src/lib.rs` files and `corpus.json` are byte-for-byte copies from
`tests/fixtures/duplex-endpoint-plugin` and `tests/fixtures/http-parity-plugin`.
Only fixture manifests are adapted: Rust edition 2024 is explicit, shared
Runtime/core dependencies use this experiment's workspace, and Capability
paths become exact released versions. No generated contract source is vendored.

`websocket.mjs` is the single dependency-free Workers upgrade adapter from
`crates/lenso-web-ingress-plugin/js/websocket.mjs` at the same owner commit.
It is also byte-for-byte identical to that file in the published
`lenso-web-ingress-plugin 0.4.3` crate. Keeping this small qualification adapter
here avoids depending on an independently published npm transport cohort.
Its frame limits, authorization boundary and close semantics are unchanged.

| Vendored file | SHA-256 |
| --- | --- |
| `duplex-endpoint-plugin/src/lib.rs` | `acbeed8295146af2efc04da753d9e52d857c7f72942097e77847aa44abd61ec5` |
| `http-parity-plugin/src/lib.rs` | `494a4494d0eaf923fb35bf5e0cfdf9aea473860112dd247adf90f0b103cd3118` |
| `http-parity-plugin/corpus.json` | `da010dc2e55eb14ec2b4aab43c6b90462e8629f66e969a757e992dc1efdbd8ca` |
| `websocket.mjs` | `b824771188e2ddc08bf8fde7156dac81f98b0fa567b347cbc9219b405b00c338` |

## Released contracts

- HTTP Endpoint `0.3.1`, HTTP Endpoint macros `0.1.4`.
- HTTP Stream Endpoint `0.1.1`, WebSocket Endpoint `0.1.0`.
- Web Ingress `0.4.3`, with default/native features disabled. The old lock
  described an unreleased local `0.4.2`; released `0.4.2` lacks the corresponding
  Wasm/duplex implementation. Released `0.4.3` supplies that implementation.
- Portable Kernel `0.3.6`, App Plan `0.4.1`, Contract Runtime `0.2.0`.

`../Cargo.lock` pins registry checksums. Runtime Driver, native adapter and facade
paths stay inside this Runtime checkout. These fixtures retain their original
package IDs, Plan bindings, routes, credentials, bytes, failure cases and session
behavior. In particular, `/stream` is GET-only; POST still returns 405.
