# Verified artifact consumption

Status: IMPLEMENTED AND VALIDATED

## Outcome

Adapters consume exactly the bytes that passed ArtifactHandle verification; path-based process and dynamic-library execution uses a stable verified artifact rather than reopening a mutable source path.

## Implementation

- `ArtifactHandle` streams one already-open source into a private stable file through a fixed 64 KiB buffer while checking the declared size and SHA-256. It retains no second in-memory artifact and performs no durability-only `fsync` for this process-local snapshot.
- Default staging uses a process-private directory chosen by the Host's OS/environment temporary policy instead of deliberately colocating with the source; an explicit Host-owned staging root provides strict isolation and supports path consumers on no-exec temporary filesystems. Process reports an actionable staging-root hint on `PermissionDenied`. The original source path remains available for its working-directory semantics.
- Process and Dylib retain the handle and execute/load its stable path directly. Remote, QuickJS, and Wasm read the stable snapshot once as their byte input.

## Validation

- Runtime-codec tests cover file drift, whole source-parent rename/replacement, a 4 MiB streamed artifact, and explicit staging. Process covers relative working directory and explicit executable staging; Dylib covers source replacement while the loaded handle keeps the stable library alive.
- Release admission benchmark: 4 MiB `13.503 ms / 296.226 MiB/s`, 64 MiB `207.706 ms / 308.128 MiB/s`, 256 MiB `1,024.021 ms / 249.995 MiB/s`.

## Boundary

This is a process-local snapshot under a Host-owned staging boundary, not a durable artifact store or a defense against a same-UID actor that also controls that staging root. Strict Hosts must pass an executable/loading-capable root outside the source materializer's authority.

Process intentionally keeps the original source parent as its child working directory for compatibility. F01 makes the executable bytes immutable; it does not snapshot ambient relative-path resources in that directory. A stricter Host should supply explicit Instance resources or a separately controlled working-directory policy.
