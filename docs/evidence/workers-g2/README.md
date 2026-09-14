# G2 buffered HTTP qualification

The shared Web Ingress event registration, generated Endpoint contract and native
parity fixture run on the qualified G1 Workers Driver and generation boundary.
The final disposable deployment is
[`lenso-workers-g2-proof`](https://lenso-workers-g2-proof.lenso.workers.dev), version
`bcf9751b-41a2-4418-9a55-204406c451f4`. This establishes the buffered Workers HTTP
path with the restrictions below. It is staging evidence, not production
Marketplace/Auth qualification or full native wire equivalence.

## Results

| Boundary | Local workerd | Deployed Workers |
| --- | --- | --- |
| Same 30-vector corpus over external HTTP | 30 host receipts pass | 27 host receipts pass; 3 edge interceptions recorded |
| Same corpus through actual Workers Request/Response objects | 30/30 pass | 30/30 pass |
| Explicit event cancellation with a healthy concurrent peer | cancelled 503, peer 200, clean shutdown | same |
| Pending HTTP closure during Wasm trap and fresh-generation recovery | pass | pass |
| Endpoint deadline | 504, clean shutdown | 504, clean shutdown |
| Oversized request body | 413; subsequent request intermittently stalls in local transport | 413; following request healthy |
| Request head bound | 431 | 431 |
| Response body bound | 502 after a real Plan-bound byte echo | same |
| Body-read timeout on a real slow Request stream inside Workers | 408 | 408 |
| Slow two-byte external upload | 408 | buffered before Worker entry; complete echo 200 |
| Actual incoming client-disconnect signal | not qualified: proxy did not forward it | qualified: Request signal aborted, invocation 503, clean shutdown |

The corpus covers methods, route precedence, raw/empty query, percent-encoded
paths, binary/empty bodies, repeated headers, Bearer and session credentials,
CSRF, forged request IDs, repeated Set-Cookie, HEAD/204/304 bodies, not-found,
method-not-allowed, domain/runtime failures and invalid responses. Every supported
response traverses Web Ingress and a Kernel Plan binding. The in-Worker corpus
constructs real Fetch Request objects and uses the same bridge as ordinary fetch;
it does not call an Endpoint directly.

The deployed client-disconnect receipt has `request_aborted: true`,
`signal: request`, `cancelled: true`, status 503 and clean App shutdown, with a
matching isolate boot identity. The probe uses the qualified G1 padded identity
stream so a small buffered response cannot make the client abort only after the
Endpoint deadline. Local failure remains visible in `local-disconnect.json`;
it is not treated as a passing local signal check.

## Transport restrictions

Cloudflare's external edge returns TRACE 405, malformed `%ZZ` paths 400 and
repeated Authorization field lines 400 before a Host receipt is produced. The
native fixture expects 200, 200 and 400 respectively. These three network results
are retained as `passed: false` with `transport-without-host-receipt`; the separate
30/30 Fetch-object corpus does not erase them.

Fetch combines repeated request fields: the observed `x-test` value is
`one, two`, rather than two independently recoverable field lines. Response
Set-Cookie remains two fields. Shared ingress rejects combined Bearer/Basic
credentials containing commas or whitespace; this is not a claim that arbitrary
Authorization parameter syntax or original field multiplicity can be recovered.
The bridge preserves path/query as supplied by Workers, including trailing `?`;
it cannot restore URL bytes the platform already normalized.

The external slow-upload result shows that the bridge's read deadline starts
when the Worker receives its stream, not when an edge starts receiving the client
upload. A genuine in-Worker ReadableStream confirms the 250 ms bridge timer.
Per-event App creation resets the current ingress request-ID sequence, so this
proof establishes replacement of untrusted IDs, not globally unique cross-event
IDs. Stream Endpoint bindings, WebSockets, CONNECT and streaming business
responses remain outside this buffered host's supported scope.

## Lifecycle and resource boundaries

The fixture uses 64 KiB request/response bodies, 16 KiB heads, a 250 ms body-read
timeout, a 500 ms Endpoint deadline, 200 ms shutdown and the 1,000 ms Runner event
boundary. These are experiment limits, not an approved production budget. JSON
byte arrays amplify memory; no full-isolate peak or large Marketplace envelope
claim follows from these small fixture results.

The shared Runner now permits a lower consumer concurrency ceiling and retirement
threshold while keeping G1's defaults: 32 active events, 64-admission idle
retirement, hard ceiling 96 and 32 bounded admission waiters. G3 may choose one
active event for its larger payloads. Invalid limits reject at startup. On
abandonment, each scope first invalidates its old Wasm closure without invoking
foreign-request native I/O. Each owning continuation then aborts and settles its
own I/O. A storage adapter must supply a bounded settlement operation; `false` or
an error becomes `503 storage_cleanup_unconfirmed`. No write is retried and a
remote side effect is not claimed rolled back.

Repeated local probing found an early rejected body could keep Wrangler's internal
HTTP connection occupied. The bridge now cancels an untouched incoming body on
head/length rejection. The focused stream-cleanup regression and subsequent
healthy-request probes cover that concrete failure. Repeated local runs, including a fresh Wrangler process, can still stall the
next request after a chunked oversized upload until the independent ten-second
transport deadline. Other fresh-process runs passed; uninterrupted recovery after
this local transport case remains unqualified. The deployed suite passes the
same request and immediate recovery check. Seven Node
boundary tests cover missing bindings, held storage at a generation deadline,
settlement errors, callback fencing before normal retirement, admission limits,
and canonical Base64 response encoding including a 3.4 MB byte roundtrip. Large
responses may use this optional encoding to reduce JSON amplification; this is
not a peak-isolate-memory qualification.

The shared Runner extraction was rechecked with G1's complete local smoke and its
12-concurrent-request I/O suite, including cross-request abandonment and fresh
network recovery. Both pass against a local upstream. An initial local-to-remote
upstream run exceeded G1's fixed one-second event deadline; the reproducible local
upstream setup avoids conflating WAN latency with the Runner regression check.
No G1 deployment was changed.

## Receipts and reproduction

- [Deployment identity and source/artifact hashes](deployment.json)
- [Local corpus and lifecycle checks](local.json)
- [Deployed corpus and lifecycle checks](remote.json)
- [Local bounds](local-limits.json) and [deployed bounds](remote-limits.json)
- [Deployed client-disconnect proof](remote-disconnect.json)
- [Local unqualified disconnect observation](local-disconnect.json)
- [Local oversized-upload transport limitation](local-transport-limitation.json)
- [G1 shared Runner smoke](g1-shared-runner.json) and [G1 I/O regression](g1-shared-runner-io.json)
- [Build, configuration and commands](../../../experiments/workers-g2/README.md)
- [Reusable Host API and scope contract](../../../experiments/workers-runtime/README.md)

The receipts contain fixture data and generated diagnostic identifiers, not
credentials. Wrangler uses the existing OAuth login. Runtime and Web changes form
a local review cohort; no Runtime or CLI package was published and
no production route or binding was modified.
