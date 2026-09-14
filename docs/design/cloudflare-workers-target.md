# Cloudflare Workers target: compatibility and implementation plan

Status: proposed design, implementation preparation only. No Plugin is certified
for Workers by this document. Production deployment and default-target changes
are out of scope until the gates below pass.

## Outcome and decisions

Run a statically composed Lenso App inside a Cloudflare Worker while preserving
Plugin identities, Capability contracts, authorization and durable invariants.
Marketplace public reads are the first product slice. Auth is a required second
slice before claiming the target supports an authenticated Marketplace.

- Keep Kernel portable. Runtime owns the Driver and event Runner; Web owns HTTP
  ingress and transport semantics; each Plugin owns its business and persistence.
- One HTTP Capability and authoring API, with native and Workers transport
  implementations. Do not introduce a Cloudflare-specific endpoint contract.
- Select an implementation for the exact deployment target before resolution.
  Keep the selection immutable in the generated Plan; no runtime fallback.
- First implementation: Rust compiled into one Wasm artifact with static Plugin
  registration. Validate existing registration before creating another Adapter.
- First lifecycle: one execution instance per event; globally cache immutable
  code, descriptors and Plan bytes only. No shared request I/O or mutable App.
- Target compatibility is an evidence-backed property of an implementation and
  its feature/dependency closure, not of the Plugin name or programming language.

## Evidence baseline

Runtime design starts from the checked-out release main. Auth was inspected at
`f9c50f5eb32ab53cd4ffd2e7588f8fdc8a8b134f`; Web at
`c22fc973c2e139e7dddec16f792723c58259784b`. These are local source snapshots, not a
claim that every sibling checkout was synchronized to its latest release.
Refresh and pin the baseline before implementation.

[Auth inventory](workers-auth-inventory.json) records direct production
dependencies for all Auth workspace members, including contracts. Dev dependencies
are excluded. It is not a Cargo resolved dependency closure or a target test.

Source findings:

| Surface | Finding | Required work / proof |
| --- | --- | --- |
| Runtime browser Driver | Uses `web_sys::window()` and browser timers | Workers event-loop and clock conformance |
| Static Plugin registry | Uses `inventory` | Wasm link/constructor/factory registration proof |
| HTTP Endpoint | `HandleRequest` / `HandleResponse` have complete byte bodies and header lists | Reuse generated bindings and shared byte-level fixtures |
| Native Web ingress | Hyper server / Tokio | Extract or reuse transport-neutral normalization and routing |
| Auth Account, API Token, Device, OAuth Flow, OIDC Provider | SQLx with Tokio/Postgres plus Postgres Kit | Inventory SQL and atomic business operations; storage strategy decision |
| Auth Password, Phone | Above plus runtime-specific execution; Password uses `spawn_blocking` | Execution and resource-budget proof without changing security parameters |
| Auth Router | Capability routing; Tokio appears in dev dependencies | Transitive target build and real contract invocation |
| Anonymous / Web Session | Random generation and dependencies on other services | Secure randomness and complete dependency closure; not automatically portable |
| OIDC Client | HTTP Client Capability | Compatible HTTP provider, secrets, randomness and transitive closure |
| Marketplace Directory / cache | Direct SQLite files | Private persistence adapters; no assumption of durable local Worker files |
| Marketplace signature verification | Re-exported through CLI dependency graph | Isolate portable verification without duplicating the wire protocol |

A dependency name alone is not an unsupported-target verdict. Feature selection,
target conditions, build dependencies and reachable code must be examined.

## Compatibility model and admission

Track five independently evidenced states: inspected, target-compiles,
contract-conformant, real-Worker-verified, product-qualified. Do not collapse them
into a boolean `supports_wasm`. `wasm32-unknown-unknown` alone says nothing about
Workers I/O, persistence, CPU budget or request ownership.

Record exact source/package version, enabled features, transitive dependency
closure, host facilities, storage guarantees, test environment, artifact digest
and limitations. Start with a report outside the public schema. Add admission
metadata through the existing authoring/implementation-selection owner only after
proving what the current schema cannot express; no parallel resolver/registry.

A deployment check must reject missing/ambiguous implementations, missing binding
names, unsupported endpoint interactions and unfulfilled durable guarantees.
Binding presence/type is a preflight check; permissions or reachability may fail
at runtime and must remain explicit failures. Never select an arbitrary fallback.

## Ownership and package seams

Runtime owns Workers scheduling and event execution, without knowledge of Auth,
D1 schemas or Marketplace. Web owns mapping the Workers request/response surface
to the existing HTTP Endpoint contract. Authoring chooses compatible
implementations and emits exact catalog/Plan/deployment metadata. Cloudflare
bindings are supplied explicitly at the event composition boundary, not through
a global mutable service locator.

Do not create a new crate for every item in this table. Create independent
packages only for real consumed contracts, runtime/transport boundaries or an
independently reusable public API. Initially validate whether native registry
mechanics work inside Wasm. If a new execution class is necessary, specify its
identity and contract projection before implementation.

Host setup differs: native binds a listener; Workers receives `fetch` events.
Plugin-facing HTTP API remains the same. Preserve the ingress logical contract
and use deployment-specific implementation/configuration; a new Plugin identity
requires evidence of a different contract, not merely a different host.

## Event lifetime and failure model

For each fetch: validate configured inputs, instantiate the lane and factories,
start Kernel, await Ready, dispatch one request, buffer a bounded response, drain
and deactivate the event-owned graph, then return the response. No handler runs
before Ready. Every event starts with its own credentials, request ID,
cancellation state, Driver tasks and storage session.

Immutable verified bytes may be cached, but their expiry must still be checked
at use. No Request, Response body, Promise carrying I/O, storage transaction or
Plugin instance is cached between events.

Start with buffered responses because that is the inspected contract. Streaming,
WebSockets, queues and scheduled triggers are separate compatibility slices;
reject unsupported interactions rather than buffer an unbounded stream or
silently change a contract. Do not use `waitUntil` as a permanent daemon.

Normal shutdown is observable and bounded. Forced CPU termination, Wasm traps
and isolate eviction may prevent cleanup. Never report clean deactivation when
there is no evidence, and never make a durable commit depend on a destructor.
If a request wrote durable state but response/cleanup failed, the domain's
idempotency or status operation must disambiguate the result before retry.

Driver timers are cooperative, not preemptive CPU deadlines. Cloudflare time
APIs advance with I/O; validate wake-up and monotonic semantics against the exact
Kernel contract. Do not fabricate elapsed time to satisfy conformance. A proven
semantic mismatch is a design gate, not a test to weaken. Wasm trapping may abort
the event rather than yield a recoverable task failure; document that distinction.

## HTTP consistency contract

Create one corpus in the Web owner and execute it against native ingress and
real Workers using the same Endpoint Plugin and contract version. Share parser /
router / error projection code where possible; transport adapters only normalize
host data and perform transport I/O.

The corpus covers:

- Methods supported by the contract, route precedence/conflicts, percent encoding,
  malformed paths, duplicate query parameters and raw query preservation.
- Binary/empty bodies, configured body bounds, absent or misleading Content-Length,
  HEAD and no-body status behavior, and unsupported interactions.
- Case-insensitive header names, repeated headers and multiple Set-Cookie values;
  document host-normalized header restrictions instead of claiming raw wire parity.
- Credential extraction, request ID treatment and existing middleware order.
  Credentials are not authenticated principals. Only the owning Auth path can
  establish identity; caller-supplied forwarded headers cannot become authority.
- Domain/Runtime failure mapping, admission before Ready, cancellation/deadline
  propagation, and two concurrent requests with different identities.

Raw TCP metadata and platform-managed headers cannot be identical. Maintain an
explicit difference table limited to transport facts. Differences in login,
authorization, cookie security, response semantics or body interpretation fail
the gate. If a contract requires unavailable information, reject that target.

## Auth portability strategy

Keep Auth Plugin IDs, operation schemas and policy owners unchanged. Extract
private, operation-oriented storage seams only where the existing code crosses
host boundaries. Avoid a generic `execute_sql` Capability or a universal database
Plugin. Shared business logic must not become a separate Cloudflare auth fork.

Choose the backend after enumerating required operations. Candidates include a
Workers-compatible access path to PostgreSQL or a dedicated D1 implementation.
Hyperdrive availability alone does not make the current SQLx/Tokio client work.
D1 is not a drop-in Postgres replacement. Portability must preserve:

| Invariant | Required concurrent/failure proof |
| --- | --- |
| Unique identity / credential creation | Concurrent duplicates cannot create conflicting subjects |
| One-time codes / OAuth state | Atomic consume; expiry and replay behavior survive retries |
| Session / token rotation and revocation | No lost updates; explicit consistency and revocation guarantees |
| Login abuse limits and attempt counters | Atomic updates; same policy under concurrency |
| Device trust / primary device | Preserve existing uniqueness and transition rules |
| Password hashing | Same format and parameters; secure salt; measured resource budget |
| Signing and secrets | Same key identity, rotation and verification behavior; no key in public bundle |

A random backend must use cryptographic entropy and fail closed. Audit the exact
`getrandom` backend/features rather than substituting a deterministic generator.
Password hashing cannot be made cheaper merely to fit Workers. If its approved
parameters exceed the target budget, mark that implementation unsupported or
explicitly compose a separately hosted compatible Auth service. Remote composition
is an alternative deployment with its own auth/transport proof, not transparent
runtime fallback.

First representative Auth test is session issuance, authenticated access and
revocation through the existing account/router/web-session composition. Add a
password path only after the hash execution feasibility gate. External OIDC login
alone is not proof that Account persistence or password auth is portable.

## Marketplace migration

Preserve Directory Capability, immutable release identities and signed envelope
bytes. Extract portable protocol/validation from CLI-specific transport and
filesystem dependencies through features or one shared protocol package if its
independent consumers justify it. Do not duplicate signature verification in JS.

Public reads first: the same Directory operation reads a published envelope via
a private Cloudflare storage adapter; the Web Plugin verifies it using the same
protocol and trust rules. D1 can hold metadata and publication pointers, R2
immutable artifacts/snapshots. Public ingress receives no signing private key.
No production data migration or signing automation is implied by the runtime MVP.

Publishing is a separate later slice: upload immutable bytes first, then atomically
publish the reviewed pointer/revision; old revision remains authoritative if the
pointer update fails. Retries must not reassign an existing version identity.
Specify expiry renewal, rollback/equivocation checks, migrations and backup before
moving authoritative writes. Switching storage is not permission to weaken them.

## Implementation gates and bounded changes

1. **G0 — target audit (first implementation task).** Refresh pinned source baselines;
   produce Cargo target/feature closures and minimal `wasm32-unknown-unknown` checks
   for Kernel, registration, HTTP contract, signature protocol, Auth Router and
   selected Auth composition. Classify blockers separately from dev-only failures.
   Deliver an evidence report with commands, exact toolchain, failures and owners.
2. **G1 — generic runtime proof.** Driver + event Runner + two statically registered
   Plugins on local workerd and deployed Workers. Prove lifecycle, invocation,
   request isolation, timers, cancellation, missing factory, trap handling and
   recreation. Measure artifact size, startup time, CPU and peak memory. If semantics
   cannot be met, stop and revise the design before product migration.
3. **G2 — HTTP parity.** Share transport-neutral behavior and run the common corpus.
   Existing native regressions remain green. Contract artifacts are generated from
   their owning source; never hand-edit bindings to make the target compile.
4. **G3 — Marketplace reads.** Same signed fixtures and error vectors on native and
   Workers, persistent data across isolate recreation, expiry and storage failures.
5. **G4 — Auth qualification.** Storage strategy ADR + representative composition;
   run the concurrency/security invariants above with real bindings and the native
   reference. No general Auth support claim before this gate.
6. **G5 — production preparation.** Domain, secrets ownership, deployment artifacts,
   migrations, rollback and observability; controlled staging, then production.

Each gate produces a scoped change in its owner repository, focused tests and an
explicit limitation report. Existing native targets remain supported. Unit mocks
or a successful Wasm compile cannot substitute for real Workers evidence.

Unresolved decisions with owners: registration support and clock semantics
(Runtime G0/G1); shared HTTP normalization and unavoidable host differences (Web
G2); storage and crypto budget (Auth G0/G4); signed snapshot persistence and renewal
(Marketplace G3/G5). No approval of these unresolved choices is assumed.

## Platform references

- https://developers.cloudflare.com/workers/runtime-apis/webassembly/
- https://developers.cloudflare.com/workers/best-practices/workers-best-practices/
- https://developers.cloudflare.com/workers/runtime-apis/performance/
- https://developers.cloudflare.com/workers/runtime-apis/context/
- https://developers.cloudflare.com/workers/runtime-apis/nodejs/fs/
- https://developers.cloudflare.com/d1/worker-api/d1-database/

Platform behavior was consulted during the design conversation. Recheck it for
the implementation's selected compatibility date and pinned tool versions.
