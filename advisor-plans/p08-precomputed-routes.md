# Precomputed immutable request routes

Status: IMPLEMENTED AND VALIDATED

## Outcome

Replicated request dispatch resolves immutable binding/instance routes at Generation construction instead of scanning Plan collections per invocation.

## Implementation

- Replicated Generation construction builds an immutable caller/capability route index containing provider count, provider identity, and lane identities. Invocation resolves only through this index and never scans Plan bindings or instances.
- The index is Generation-owned and preserves the existing missing, singular, and ambiguous-binding errors.

## Validation

- Dedicated tests cover missing, one-provider, and two-provider ambiguity semantics plus a self-contained 512-binding index.
- Synthetic five-million-lookup release microbenchmark: one binding `10.001 ns/lookup`, 8,192 bindings `49.918 ns/lookup` (`4.992x`). Together with the Generation-owned index test, this proves invocation no longer scans the Plan and characterizes indexed lookup scaling; it is not an old-linear-scan comparison or end-to-end throughput result.
