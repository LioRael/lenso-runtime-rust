# Cross-lane request transfer cost

Status: IMPLEMENTED AND VALIDATED

## Outcome

Reduce fixed per-request allocation and cloning on the cross-lane path without changing immutable Plans or single-owner lane semantics.

## Implementation

- The normal cross-lane request path now uses `try_send`; it constructs and polls an async send future only on the slow/error path when the bounded lane queue cannot accept immediately (full or closed). Immutable diagnostic provider text is shared as `Arc<str>` instead of cloned as an owned `String` per transfer.
- Immutable Plans, single-owner lanes, cancellation, deadlines, and the no-work-stealing rule are unchanged.

## Validation

- Existing lane/request/terminal/shutdown suites pass, including cross-lane cancellation and failure paths.
- Local release microbenchmark median: sequential cross-lane `62,793 req/s` (ratio `0.024`), concurrent-64 cross-lane `534,768 req/s` (ratio `0.252`). Against the checked directional baseline, concurrent cross-lane throughput rose from about `442,561 req/s` (+20.8%); sequential throughput remained within noise. This is fixed-path evidence, not an end-to-end product throughput guarantee.
