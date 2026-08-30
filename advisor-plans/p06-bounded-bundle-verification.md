# Bounded plugin bundle verification

Status: IMPLEMENTED AND VALIDATED

## Outcome

Bundle verification enforces host-owned file-count, per-file, and total-byte limits and computes digests without retaining the entire directory in memory.

## Implementation

- Default `BundleVerificationLimits` bound the manifest to 1 MiB, each file to 256 MiB, total bytes to 512 MiB, regular files to 128, all directory entries to 256, and directory depth to 32. Every `DirEntry`, including an empty directory, consumes the entry budget.
- Verification walks and hashes through a fixed 64 KiB buffer. Peak retained content is summaries plus the manifest and, when required, at most one bounded Wasm artifact for descriptor extraction.
- `read_bundle_manifest` parses the initially read bounded manifest bytes only after comparing them with the independent directory-walk summary, avoiding a third unverified reread. Opened file identity is checked against inspected metadata, closing the direct lstat-to-open file/symlink swap on supported Unix hosts.

## Validation

- Seven bundle tests pass, including oversized bytes/files, many empty directories, manifest replacement, direct symlink replacement, digest closure, and valid bundles.

## Boundary

This verifier assumes Host-owned, quiescent Bundle materialization. It does not claim immunity to a same-UID adversary concurrently replacing directory components or mutating material after return. Reopen this design if verification must run over an actively mutable untrusted directory; that requires a private snapshot or directory-FD/openat-style traversal with no-follow enforcement for every component.
