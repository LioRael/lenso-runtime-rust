# Deep improvement implementation plans

Generated from the 2026-08-30 deep audit and implemented in the isolated
`codex/deep-improvements-20260830` worktree before repository delivery.

## Execution order and status

| Plan | Title | Status |
|---|---|---|
| F01 | Verified artifact consumption | DONE |
| F05 | Durable staged-generation rollback | DONE |
| F06 | Process invocation deadline cleanup | DONE |
| P03 | Cross-lane request transfer cost | DONE |
| P06 | Bounded Plugin Bundle verification | DONE |
| P07 | Bounded Remote Adapter worker model | DONE |
| P08 | Precomputed immutable request routes | DONE |
| Edge | Accepted stream guest cleanup | DONE |
| Edge | Remote redirect policy authority | DONE |
| Edge | UTF-8-safe Adapter failure bounds | DONE |

## Boundary and review state

Every plan records its focused tests and measurements. A final independent
source review approved the implementation after closing Remote cancel-overflow
and Artifact source-parent replacement races. The affected packages pass tests,
formatting, check, all-target Clippy with warnings denied, and `git diff --check`.

The remaining boundaries are explicit rather than deferred implementation:

- Bundle verification requires Host-owned, quiescent materialization; an
  actively hostile same-UID directory requires a private snapshot or
  directory-FD/no-follow traversal.
- Strict path consumers must use a Host-owned executable/loading-capable
  Artifact staging root. Process working-directory resources remain ambient.
- The built-in Remote client rejects redirects; an injected product-owned
  client deliberately owns its redirect policy.
- Remote shutdown raises an invoke-stop flag instead of draining abandoned
  queue backlog. Priority cancellation takes at most two one-second cancel
  rounds for four dispatched requests; final join can still wait for the
  remaining timeout of already-dispatched HTTP requests that ignore cancellation.
