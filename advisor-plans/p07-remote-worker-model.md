# Bounded remote adapter worker model

Status: IMPLEMENTED AND VALIDATED

## Outcome

Remote invoke and cancel operations use a bounded reusable execution model instead of one OS thread and redundant JSON conversion per request.

## Implementation

- Per Remote Generation, four steady-state invoke workers consume a queue bounded by `max_pending_requests`; a queued request is atomically confirmed pending before HTTP dispatch and queue wait consumes its request-timeout budget.
- Request JSON is validated with `RawValue` and serialized once. Per-work state deduplicates cancellation, while a bounded normal-cancel queue and two fixed cancel workers fail the Generation closed on saturation. Saturation records every already-dispatched ID as uncertain before abandoning pending state, so shutdown cannot lose them.
- Shutdown first raises a shared invoke-stop flag, so workers exit after any already-dispatched HTTP request instead of dequeuing abandoned `Execute` backlog. It independently gives each of at most four uncertain dispatched IDs one attempt through at most two scoped priority-cancel workers; each worker handles at most two one-second requests, even if the normal cancel pool is occupied. The remaining join is therefore bounded by already-dispatched request timeouts rather than `max_pending_requests` queue-drain work.
- Guest/provider failure text is truncated at a UTF-8 character boundary.

## Validation

- Remote tests cover real overlap, queued cancellation without dispatch, queue-wait timeout, single cancellation, bounded slow-cancel overflow, all-dispatched priority shutdown under a saturated cancel pool, redirect authority, request/response bounds, and non-ASCII failure details.
- The real shutdown fixture blocks all four invoke workers, queues 128 additional invokes, fills the normal cancel queue behind two blocked steady-state cancel workers, and triggers overflow. It observes all four exact dispatched IDs through one priority attempt each, proves every queued outcome is dropped rather than worker-drained, and completes in about `1.00 s`; with two scoped shutdown workers and at most two serial requests each, the priority phase is bounded near `2 s`, plus any remaining already-dispatched `request_timeout`.
