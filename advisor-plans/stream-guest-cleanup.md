# Accepted stream guest cleanup

Status: IMPLEMENTED AND VALIDATED

## Outcome

A provider stream remains owned by its cleanup watcher after guest acceptance and is cancelled/removed when the accepted guest handle is later dropped.

## Implementation

The watcher now treats acceptance as a state transition, continues waiting for a later cancellation signal, and is the single provider-cleanup dispatcher. This also preserves pre-accept abandonment cleanup without double dispatch.

## Validation

The real replicated interaction test observes provider cancellation for both pending-open abandonment and `open -> successful guest handle -> drop without explicit cancel`. Source review confirms that the same watcher then removes the map entry, and a new session opens successfully afterward.
