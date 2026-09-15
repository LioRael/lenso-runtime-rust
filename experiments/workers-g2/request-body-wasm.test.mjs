// Requires freshly built G2 bindings. Missing artifacts/dependencies fail the
// suite; native decoder tests and JS mocks are not a substitute for this path.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import * as bindings from "./pkg/lenso_workers_g2_host.js";
import { clearTimers } from "./clock.mjs";
import { createEventRunner } from "../../packages/workers-runtime/runner.mjs";
import { createHttpHandler, createStreamingHttpHandler } from "../../packages/workers-runtime/http.mjs";

const module = new WebAssembly.Module(await readFile(new URL("./pkg/lenso_workers_g2_host_bg.wasm", import.meta.url)));
const runner = createEventRunner({
  instantiate: () => bindings.initSync({ module }),
  resetState: bindings.__wbg_reset_state,
  clearTimers,
  maxConcurrent: 1,
});

function handlers(requestBodyEncoding, wire) {
  const options = {
    requestBodyEncoding,
    maxRequestBodyBytes: 65536,
    maxRequestHeadBytes: 16384,
    bodyReadTimeoutMs: 250,
  };
  return {
    buffered: createHttpHandler({
      ...options, run: runner.run, maxResponseBodyBytes: 65536,
      handleHttp: (input, scope) => bindings.handle_http(wire ?? input, scope),
      onReceipt: (receipt, response) => response.headers.set("x-g2-shutdown", receipt.shutdown),
    }),
    streaming: createStreamingHttpHandler({
      ...options, open: runner.open,
      async openHttp(input, scope) {
        const response = await bindings.open_http(wire ?? input, scope);
        return {
          value: { status: response.status, headers: JSON.parse(response.headers), read: () => response.read() },
          closed: response.closed,
        };
      },
    }),
  };
}
const post = (bytes) => new Request("https://proof.invalid/bytes", { method: "POST", body: bytes });
const stream = () => new Request("https://proof.invalid/stream");
const bytesOf = async (response) => new Uint8Array(await response.arrayBuffer());

for (const encoding of ["numeric-array", "base64-v1"]) {
  const host = handlers(encoding);
  test(`${encoding}: actual G2 byte echo and session responses`, { timeout: 30000 }, async () => {
    for (const bytes of [
      new TextEncoder().encode("你好 🌍 café\u0000"),
      ...[0, 1, 2, 3, 256, 12287, 12288, 12289, 65534, 65535, 65536]
        .map((size) => Uint8Array.from({ length: size }, (_, i) => i % 256)),
    ]) {
      const response = await host.buffered(post(bytes));
      assert.equal(response.status, 200);
      assert.equal(response.headers.get("x-g2-shutdown"), "clean");
      assert.deepEqual(await bytesOf(response), bytes);
      // The duplex fixture only registers GET /stream. Exercise preparation of
      // a nonempty session request through its existing method rejection too.
      const rejected = await host.streaming(new Request("https://proof.invalid/stream", {
        method: "POST", body: bytes,
      }));
      assert.equal(rejected.status, 405);
      await rejected.arrayBuffer();
    }
    const response = await host.streaming(stream());
    assert.equal(response.status, 200);
    assert.deepEqual(await bytesOf(response), new Uint8Array([0, 1, 255, 2, 3, 254]));
  });

  test(`${encoding}: a real held session rejects both transports before reading or encoding`, { timeout: 5000 }, async (t) => {
    const held = await host.streaming(new Request("https://proof.invalid/stream?hold"));
    assert.equal(held.status, 200);
    const encode = t.mock.method(globalThis, "btoa");
    try {
      for (const handle of [host.buffered, host.streaming]) {
        let reads = 0, readers = 0, cancels = 0;
        const body = new ReadableStream({
          pull() { reads++; }, cancel() { cancels++; },
        }, { highWaterMark: 0 });
        const getReader = body.getReader.bind(body);
        body.getReader = (...args) => { readers++; return getReader(...args); };
        const response = await handle(new Request("https://proof.invalid/bytes", {
          method: "POST", body, duplex: "half",
        }));
        assert.equal(response.status, 503);
        assert.deepEqual(await response.json(), { error: "host_unavailable" });
        assert.deepEqual({ reads, readers, cancels }, { reads: 0, readers: 0, cancels: 1 });
      }
      assert.equal(encode.mock.callCount(), 0);
    } finally {
      await held.body.cancel();
    }
    const healthy = await host.buffered(post(new Uint8Array([0, 255])));
    assert.equal(healthy.status, 200);
    assert.deepEqual(await bytesOf(healthy), new Uint8Array([0, 255]));
  });

  test(`${encoding}: existing body and head error contracts`, async () => {
    for (const handle of [host.buffered, host.streaming]) {
      const oversized = await handle(post(new Uint8Array(65537)));
      assert.equal(oversized.status, 413);
      assert.deepEqual(await oversized.json(), { error: "payload_too_large" });
      const head = await handle(new Request("https://proof.invalid/bytes", { headers: { "x-large": "x".repeat(16384) } }));
      assert.equal(head.status, 431);
      assert.deepEqual(await head.json(), { error: "request_header_fields_too_large" });
    }
  });
}

test("both actual Rust entry points reject malformed envelopes with bounded HTTP errors and recover", { timeout: 30000 }, async () => {
  const head = { method: "POST", uri: "/bytes", headers: [] };
  const malformed = [
    ...["AB==", "AAB=", "AA", "AAA", "AA-_", "AA==\n", "AA ==", "====", "A===", "AA=A", "AA==AAAA", "éAAA"]
      .map((body_base64) => ({ body_encoding: "base64-v1", body_base64 })),
    {}, { body_base64: "" }, { body_encoding: "base64-v1" },
    { body_encoding: "base64-v2", body_base64: "" },
    { body: [], body_encoding: "base64-v1", body_base64: "" },
    { body: null, body_encoding: "base64-v1", body_base64: "" },
    { body: [], body_base64: null }, { body: [], body_encoding: null },
    { body: [], extra: true }, { body_encoding: "base64-v1", body_base64: "", extra: true },
    { body_encoding: "base64-v1", body_base64: null },
    { body: [256] }, { body: [-1] }, { body: [1.5] },
    { body: Array(65537).fill(0) },
    ...[65537, 65538, 65539].map((size) => ({ body_encoding: "base64-v1", body_base64: Buffer.alloc(size).toString("base64") })),
  ].map((fields) => JSON.stringify({ ...head, ...fields }));
  malformed.push(
    '{"method":"POST","uri":"/bytes","headers":[],"body":[],"body":[]}',
    '{"method":"POST","uri":"/bytes","headers":[],"body_encoding":"base64-v1","body_base64":"","body_base64":""}',
    " ".repeat(65536 * 4 + 16384 * 6 + 1),
  );
  for (const wire of malformed) {
    for (const handle of Object.values(handlers("numeric-array", wire))) {
      const response = await handle(post(new Uint8Array()));
      assert.equal(response.status, 503);
      assert.equal(response.headers.get("cache-control"), "no-store");
      assert.equal(response.headers.get("x-content-type-options"), "nosniff");
      assert.deepEqual(await response.json(), { error: "host_unavailable" });
      const healthy = await handlers("base64-v1").buffered(post(new Uint8Array([255])));
      assert.equal(healthy.status, 200);
      assert.deepEqual(await bytesOf(healthy), new Uint8Array([255]));
    }
  }
});
