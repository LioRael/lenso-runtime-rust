# Durable staged-generation rollback

Status: IMPLEMENTED AND VALIDATED

## Outcome

Every failure after staging but before durable Active publication/live-slot adoption attempts shutdown of each staged Generation exactly once.

## Implementation

- Transition and recovery retain ownership of every staged handle until durable Active publication/live-slot adoption. Every intermediate error makes exactly one best-effort shutdown call for each owned candidate.
- Cleanup is best-effort and never replaces the primary transition/store/authority/parse error.

## Validation

- Failure-injection tests assert exact stage/shutdown events for Ready CAS failure (`fail_on(3)`), final Active CAS failure (`fail_on(4)`), post-stage routing-epoch overflow, recovery authority failure, recovery parse failure, and final recovery CAS failure. Cleanup-failure cases also prove that the original error wins.
- `lenso-plugin-control-plane` tests: 8 passed; affected check and Clippy gates passed.
