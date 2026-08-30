# Remote redirect policy authority

Status: IMPLEMENTED AND VALIDATED

## Outcome

The built-in Remote transport never follows redirects implicitly; a product Host may deliberately supply a client with a different policy.

## Implementation

The default client uses `Policy::none()`. `with_http_client` documentation explicitly transfers proxy, identity, mTLS, and redirect-policy authority to the product Host instead of claiming a global adapter invariant.

## Validation

One real HTTP test proves the default readiness request rejects a redirect, and another proves a custom product-owned client can explicitly follow one.
