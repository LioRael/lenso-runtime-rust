# UTF-8-safe adapter failure bounds

Status: IMPLEMENTED AND VALIDATED

## Outcome

Untrusted guest/provider failure details cannot panic a Host when their byte limit falls inside a multi-byte UTF-8 character.

## Implementation

Process and Remote bound failure details to 512 bytes, and Dylib, QuickJS, and Wasm bound them to 1,024 bytes, always walking back to a valid character boundary before truncation.

## Validation

Real Process and Remote provider tests exercise non-ASCII failures across their transport boundaries. Dylib, QuickJS, and Wasm unit tests cover the shared boundary case; all five adapters pass tests and `--all-targets -D warnings` Clippy.
