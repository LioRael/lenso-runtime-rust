# Lenso Remote HTTP JSON V1

Protocol identifier: `lenso.remote-http-json@1`.

All responses use HTTP 200 with `application/json` when a protocol envelope was
produced. Transport, authentication, routing, and deployment failures use their
normal non-200 HTTP status. The Adapter treats non-200 status as a runtime
failure and malformed envelopes as a protocol violation.

## Readiness

`GET <base_url>/lenso/v1/ready`

```json
{
  "protocol": "lenso.remote-http-json@1",
  "descriptor": {
    "abi": "lenso.json-request@1",
    "capabilities": [
      {
        "capability_id": "example.echo@1",
        "descriptor_version": "1.0.0",
        "request_operations": ["echo"],
        "stream_operations": []
      }
    ]
  }
}
```

The descriptor must exactly match the resolved Plugin Instance before the
Generation becomes ready. Remote V1 rejects Host imports and Streams.

## Invocation

`POST <base_url>/lenso/v1/invoke`

```json
{
  "protocol": "lenso.remote-http-json@1",
  "generation_id": "3a78c67a-7838-4c27-ae05-c554ecb5b97b",
  "request_id": 1,
  "capability": "example.echo@1",
  "operation": "echo",
  "request": {"message": "hello"}
}
```

Return exactly one of `ok`, `error`, or `failure`. Echo the exact protocol,
generation identity, and request identity:

```json
{
  "protocol": "lenso.remote-http-json@1",
  "generation_id": "3a78c67a-7838-4c27-ae05-c554ecb5b97b",
  "request_id": 1,
  "ok": {"message": "hello"}
}
```

`ok` is a typed Capability response, `error` is a declared typed Domain Error,
and `failure` is a bounded operational failure string. The Host never retries
an invocation because the Adapter cannot infer whether remote side effects were
committed.

## Cancellation

`POST <base_url>/lenso/v1/cancel`

```json
{
  "protocol": "lenso.remote-http-json@1",
  "generation_id": "3a78c67a-7838-4c27-ae05-c554ecb5b97b",
  "request_id": 1
}
```

Cancellation is best effort. The service should stop work for the exact
Generation/request pair and may return any bounded HTTP response. A cancelled
Host invocation remains cancelled even if the invocation response wins later.

## Security and limits

The Adapter requires HTTPS except for loopback development and rejects URLs
containing credentials, query strings, or fragments. The Host owns readiness
and request timeouts, maximum concurrent requests, and maximum request and response bytes.
Remote V1 does not define application authentication; deploy behind a product
owned authenticated transport boundary. A Host may inject a configured HTTP
client for proxy, default headers, or mTLS without placing secrets in the
digest-verified deployment binding.
