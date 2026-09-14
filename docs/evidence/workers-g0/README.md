# Workers G0: production dependency and target audit

Status: baseline audit complete; Workers runtime compatibility is NOT established.
Nine locked production-library checks ran with Rust 1.94.0 and
`wasm32-unknown-unknown`. No production code, generated contract or dependency
feature was modified to make these checks pass. Audited branches were clean;
the Runtime branch additionally contains this design/evidence work.

## Results

| Check | Result | Interpretation / owner |
| --- | --- | --- |
| Kernel 0.3.5 + Native Adapter 0.3.13 + Browser Driver 0.1.8 | PASS | Typechecks for target. Does not establish static registration at link/runtime, nor Window-free execution. Runtime owns next proof. |
| HTTP Endpoint 0.3.1 | PASS | Existing business HTTP contract compiles, using Kernel 0.3.6 in this workspace. Web owns shared behavioral fixtures. |
| Auth Router 0.1.0 | PASS | A real production Auth Plugin compiles; test-only Tokio did not pollute the check. No Auth provider or complete authentication flow was exercised. |
| Native Web Ingress 0.4.2 | FAIL | Mio 1.2.3 via Tokio/Hyper requires native network support. Web must replace the transport while preserving the contract. |
| Auth Account 0.1.0 | FAIL | getrandom 0.2.17 backend via ring/rustls/SQLx/Postgres. Auth owns storage/transport extraction; enabling random support alone is not sufficient proof. |
| Auth Password 0.1.0 | FAIL | Same first random backend blocker; source also uses `spawn_blocking`. Auth owns execution strategy and unchanged security budgets. |
| Auth Web Session 0.1.0 | FAIL | Direct getrandom 0.4.3 lacks target backend. Audit its backend separately from database-dependent Auth Plugins. |
| Auth OIDC Client 0.1.0 | FAIL | getrandom 0.2.17 via jsonwebtoken / cryptography, not the SQLx path. Exact JWT algorithm and entropy configuration need target proof. |
| Signature consumer: CLI 0.5.3, no default features | FAIL | getrandom 0.2.17 via ureq/rustls/ring remains in the library closure. This rejects the current packaging boundary; it does NOT prove signature algorithms cannot compile. |

The Account + Password + Web Session combined check also failed before Plugin
code. Individual checks above avoid attributing a shared failure to every Plugin.
Failures are the first observed compiler barriers, not an exhaustive list of all
later errors. No random feature override, alternate crypto implementation or
weakened authentication rule was applied during this audit.

## Exact evidence and reproducibility

[results.json](results.json) records each exact source HEAD, lockfile digest,
check arguments, exit status, diagnostics and raw log / feature-tree digests.
[Auth path](auth-random-path.txt), [OIDC path](oidc-random-path.txt),
[Ingress path](ingress-mio-path.txt) and [CLI path](cli-random-path.txt) are captured
inverse production dependency trees. Full logs and feature closures from this run
are retained in the local `/tmp/lenso-workers-g0/` evidence directory.

The feature trees use `-e normal,build,features`: proc macros and code generators
are host build dependencies, not automatically Worker runtime dependencies.
Dev dependencies are excluded. Do not infer target support from an inventory
entry or a crate name alone. The earlier design inventory used older Auth/Web
local snapshots; this report supersedes that baseline for audited components.
It does not claim to have compiled every Plugin in the workspace family.

Reproduce against checkouts of the recorded HEADs with unchanged Cargo.lock files:

```sh
python3 scripts/audit-workers-target.py \
  --runtime "$RUNTIME_ROOT" --web "$WEB_ROOT" \
  --auth "$AUTH_ROOT" --cli "$CLI_ROOT" \
  --cargo "$CARGO" --output "$AUDIT_OUTPUT"
```

On the local framework workspace, set `CARGO` to its `lenso-cargo` wrapper.
Elsewhere, use `cargo`. `--case auth-web-session` can isolate one check. The
collector records compile failures as results; its success exit is not a passing
compatibility verdict. Inspect each check AND feature-tree status in report.json.
Rust 1.94.0 and its wasm32-unknown-unknown standard library were already installed.
The script help/syntax were checked; the recorded baseline commands were executed
individually, not by silently replacing their outcomes with a later collector run.

## Implementation decision

Proceed to a scoped G1 runtime prototype, with these prerequisites made explicit:

1. Pin one compatible Kernel/contract cohort for the prototype. This audit preserves
   each owner's lockfile and observes Kernel 0.3.5 and 0.3.6; it does not claim one
   combined App has been linked against them.
2. Prove a linked Wasm artifact contains and discovers two generated Plugin
   factories. `cargo check` is not linker-constructor or registration evidence.
3. Implement Workers event scheduling and lifetime around existing Kernel semantics;
   browser Driver compilation does not make `Window` available in Workers.
4. Reuse the existing HTTP contract. Isolate Web transport code rather than letting
   native listener features enter the Worker artifact. Add one shared corpus before
   migrating product routes.
5. Isolate portable signed-catalog verification from CLI authoring/download/filesystem
   dependencies. Evaluate a narrow feature boundary before creating a new package.
6. Treat Auth as separate qualified slices: random/JWT backends, database operations,
   password execution, and end-to-end authentication semantics. Do not broaden the
   supported-target claim after fixing only the first compiler error.

No live Cloudflare execution, HTTP parity, secure-random execution, password cost,
transaction concurrency, storage migration or production deployment is proved by
G0. These remain the G1–G5 gates in the design.
