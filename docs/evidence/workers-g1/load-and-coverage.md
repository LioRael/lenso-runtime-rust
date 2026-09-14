# Workers G1 load and coverage follow-up — 2026-09-14

Added real-host assertions for unsupported Plan schema, operation-table mismatch during preparation, and the exact Unavailable failure for an absent binding. These follow pinned conformance 0.3.2 vectors and reuse its adapter, not a second implementation of validation rules.

The [complete upstream test inventory](conformance-matrix.md) makes remaining gaps explicit. Covered means behavior exercised in the Workers fixture, not a claim that the original deterministic harness was run unchanged.

## Validation

Locked release build, all-features target Clippy with warnings denied, formatting, and [local](local-coverage-smoke.json)/[remote](remote-coverage-smoke.json) full smoke passed. The smoke checks that the three new vectors are present, so an old deployment cannot satisfy the updated check.

- Worker version: `f7fe2b3c-8d68-40ed-a0b1-b62b523a7e83`.
- Wasm SHA-256: `7c9644268f1323f20f00872dd9fcbe22f57ece1f67df5fcb7afb08cad3e7e1f4`.
- Upload: 1559.81 KiB; gzip: 461.23 KiB. Wrangler startup: 1 ms, not request CPU.

## Reproducible load

Each closed-loop run uses 240 requests, 12 concurrent clients, unique Unicode inputs and real startup/invocation/shutdown. Every response passed identity, invocation-count and Clean-shutdown checks. No fault suite overlapped these remote load windows. The public diagnostic endpoint is not access-isolated, so unrelated requests cannot be categorically excluded.

| Run | Client p50 / p99 ms | Requests/s | Observed isolate boot IDs | Maximum sampled Wasm capacity bytes |
| --- | --- | --- | --- | --- |
| [local-load](local-load.json) | 9.2 / 61.3 | 900.5 | 1 | 3670016 |
| [remote-load](remote-load.json) | 141.7 / 872.4 | 66.5 | 6 | 2621440 |
| [remote-io-load](remote-io-load.json) | 339.8 / 893.2 | 32.3 | 7 | 3670016 |

Client latency includes network and connection setup. The delayed I/O run includes a 200 ms controlled upstream delay. Local and remote numbers are not comparable CPU benchmarks. Wasm capacity is observed after requests and excludes live-allocation accounting, JS heap and total isolate peak memory. These short synthetic runs are not a production SLA or sustained-load certificate.

## Remote analytics

The repeatable `metrics.py` collector uses the [official Workers metrics query](https://developers.cloudflare.com/analytics/graphql-api/tutorials/querying-workers-metrics/) with script and time filters. Each receipt includes the exact expanded query window and load run identity. It retains CPU quantiles in API-native units and reports expected versus observed request counts.

The [normal sample](remote-load-metrics.json) and [I/O sample](remote-io-load-metrics.json) are incomplete (`count_matches: false`). They must not be presented as complete per-run CPU baselines. Delayed ingestion, sampling, timestamp granularity and unrelated public traffic limit attribution. The initial subsecond-boundary query returned fewer rows; whole-second query bounds reduce that edge but do not resolve the remaining count discrepancy.

## Gate decision

G1 remains open. Next work is the named dependency/capacity and diagnostic vectors identified in the inventory, followed by stream/event qualification or an explicitly enforced narrower target profile. A representative CPU baseline and total peak-memory evidence remain separate requirements. Do not promote the experimental Driver into supported packages or claim native HTTP ingress parity from these diagnostic probes.
