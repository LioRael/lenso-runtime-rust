# Process invocation deadline cleanup

Status: IMPLEMENTED AND VALIDATED

## Outcome

Dropping an unresolved admitted Process invocation removes its pending entry and synchronously retires the child generation so a late response cannot be mis-correlated, without attempting a pipe write/flush from the destructor.

## Implementation

- A request-scoped guard aborts only when it successfully removes an unresolved pending entry. If the reader already settled and removed that entry, dropping the future leaves the healthy Generation running.
- Abandonment marks the Generation failed, stops the child, and retires remaining pending requests; explicit cancellation retains its existing request-level result semantics.
- Guest-controlled failure details are truncated only at UTF-8 character boundaries.

## Validation

- Seven real-process tests cover ordinary request/shutdown, cooperative cancellation, unresolved abandonment, already-settled future drop, relative source working directory, explicit executable staging, and non-ASCII failure truncation.
- Existing Kernel test `deadline_stops_one_native_call_without_retrying_it` proves deadline selection drops the admitted provider future; the Process abandonment test proves the corresponding adapter cleanup.
- Process tests and `--all-targets -D warnings` Clippy pass with `test-fixture` enabled.
