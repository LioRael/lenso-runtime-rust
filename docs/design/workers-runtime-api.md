# Workers runtime API and complete target qualification

Status: implementation in progress. This document is a work ledger, not acceptance evidence.

## Ownership

The Runtime owns event resource lifetimes, generation leases, Wasm reset and build
assets. The native Adapter owns explicit factory selection and generated public
construction hooks. Web owns HTTP, streams, WebSocket transport and their public
Capability contracts. Auth owns each method's rules and private persistence.
Portable Kernel policy, Plugin identities and authorization stay unchanged.

The [W02 mixed-session Host/profile decision](workers-session-host-profile.md)
selects separate short-request and session Worker deployments, each owning one
generated bindings/Runner/clock domain. It preserves the existing public APIs
and per-event App ownership. This is an architecture decision, not an implemented
or qualified mixed-workload profile.

## Required delivery

1. Package the event runtime and Rust Workers Driver independently of experiments.
   Provide one resource scope that automatically tracks native operations, fences
   Rust callbacks before reset, aborts only in the owner context and bounds cleanup.
2. Expose generated public construction/endpoint projection and explicit factory
   replacement. Reject duplicate overrides and missing identities before Ready;
   linked registration order must not select an implementation.
3. Supply one package build entry point with generated linkage/reset validation.
   Prove unpacked package use outside the sibling repository layout; migrate Auth
   and Marketplace consumers away from experimental source imports.
4. Add operation-oriented Workers storage for Password, Phone, Device, API Token
   and OIDC Provider. Preserve password cost, rate limits, one-use state, revocation,
   expiry, grant scope and native behavior. Run real PostgreSQL and D1 compositions,
   actual HTTP flows, restart, failure/cancellation and package-containment checks.
5. Extend Web event ingress to the existing Stream Endpoint contract. Add a
   platform-neutral WebSocket session contract with native and Workers transports.
   Keep the App alive through terminal stream/socket closure, bound queues and
   frame sizes, propagate backpressure/cancellation, and prevent reset of live leases.

## Acceptance

Native defaults must remain supported. A successful Wasm build is not platform
qualification. Every target claim needs actual workerd and deployed Worker proof,
failure and recovery receipts, plus declared platform limitations. Provider SMS
fixtures and controlled OIDC peers do not imply third-party service qualification.
Production rollout, database migration and registry publication are separate actions.

Mixed-session qualification additionally requires the
[W02 acceptance matrix](workers-session-host-profile.md#executable-acceptance-matrix-for-implementation),
including a held session across more than 96 short requests, route partitioning,
capacity and cleanup bounds, peer failures and same-domain recovery. Existing
duplex and buffered evidence does not close W02. Its
[implementation prerequisites](workers-session-host-profile.md#staged-migration-and-exact-implementation-prerequisites)
include transport reservation before buffering and bounded cleanup quarantine;
these remain follow-up work, with no public API change authorized here.

## Current progress (2026-09-15)

- `packages/workers-runtime` and `crates/lenso-workers-driver` now own the promoted
  implementation. The npm archive builds the Auth proof Host without sibling JS
  imports; the Rust Driver archive also compiles after extraction outside the repository.
  All seven Auth owner archives compile using only extracted Runtime/Capability
  archives. Registry publication remains separate.
- Shared event scope, public configured factories and explicit overrides are
  implemented. Generated package `link_plugin()` functions retain private Plugin
  types. Configured factories currently support authoring v1 only.
- G1/G2 Hosts now use the Driver crate and package build entry; Wasm checks pass.
  G2's unpublished parity fixture is supplied by an explicit external Cargo config.
  Marketplace Host migration and Wasm check pass. Its six storage/fencing
  tests and package build pass. G1's package build and local workerd smoke pass
  (26 checks, including 12 concurrent requests). Unpublished cohorts
  install supplied npm archives with `--no-save --package-lock=false`; release
  version coordination and registry-backed lockfiles belong to publication.
- Five method owners now have private D1 adapters. Existing real PostgreSQL tests
  for the five owners pass (33 tests). Dedicated deployed Worker
  `ff78dedc-5803-4679-b6d0-3c9ded68f95a` passed 36 method checks, 22 method failure
  checks and all 59 earlier Account/OAuth/WebSession regressions (117 total).
  Evidence lives in Auth `experiments/workers-g4/evidence/*methods*.json`,
  `method-failures.json` and `*-runtime-api.json`. SMS and upstream IdP are fixtures.
- The real-D1 run found and fixed a Device SQLite reserved-word alias and boolean
  binding conversion. The composition proof now groups Many bindings explicitly.
- Native/event ingress share response frame validation. Five native/event stream
  tests pass, including failed HTTP chunk termination. The JS package has 23
  resource/HTTP/session tests. Real Workers HTTP streams deliver binary bytes
  before terminal completion and support consumer cancellation.
- WebSocket Descriptor and Rust projection are generated. TypeScript projection
  typechecks against published `@lenso/contract-runtime` 0.3.0 in an isolated
  directory. Native and Workers share route, handshake, frame and authorization
  policy. Four Node transport tests cover bounded queues and late events. Three
  native/event duplex tests pass, including retaining request admission until
  stream release. The full Web Ingress test suite passes (61 tests).
- Dedicated Worker `930871c0-2a02-4edf-bdcd-1faa1e5a5857` passed nine network
  qualification cases for binary streaming, cancellation, WebSocket text/binary/
  empty messages, protocol selection, close, authorization, Origin rejection,
  oversized messages, peer disconnect and concurrent-session recovery.
  The same nine cases pass under local workerd. Generated
  Wasm class methods and finalizers fence obsolete instance IDs. Workers send()
  has no drain API: output is byte-bounded, not acknowledged network backpressure.
  WebSocket hibernation is outside this event-runtime scope.

No current implementation PR, merge or registry publication has been performed.
The new proof Worker is isolated from production and earlier G4 resources.
