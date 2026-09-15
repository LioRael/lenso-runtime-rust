import assert from "node:assert/strict";
import test from "node:test";
import { createHttpHandler, createStreamingHttpHandler } from "../http.mjs";
import { createWorkersHttpHost } from "../host.mjs";
import { createEventRunner } from "../runner.mjs";

function bindings(handle_http) {
  return {
    initSync: () => ({ memory: new WebAssembly.Memory({ initial: 1 }), __wasm_call_ctors() {} }),
    __wbg_reset_state() {},
    handle_http,
  };
}

for (const kind of ["buffered", "streaming", "host"]) {
  function handler(encoding, receive) {
    const generated = bindings((input) => {
      receive(input);
      return JSON.stringify({ status: 200, headers: [], body: [0, 255], shutdown: "clean" });
    });
    const options = encoding === undefined ? {} : { requestBodyEncoding: encoding };
    if (kind === "host") return createWorkersHttpHost({
      bindings: generated, wasmModule: {}, ...options,
      limits: { maxRequestBodyBytes: 65536 },
    }).fetch;
    const runner = createEventRunner({
      instantiate: generated.initSync, resetState: generated.__wbg_reset_state, clearTimers() {},
    });
    if (kind === "buffered") return createHttpHandler({
      run: runner.run, handleHttp: generated.handle_http, maxRequestBodyBytes: 65536, ...options,
    });
    return createStreamingHttpHandler({
      open: runner.open, maxRequestBodyBytes: 65536, ...options,
      openHttp(input, scope) {
        receive(input);
        let finish, sent = false;
        const closed = new Promise((resolve) => (finish = () => resolve({ shutdown: "clean" })));
        scope.attach(finish);
        return { closed, value: { status: 200, headers: [], read() {
          if (!sent) { sent = true; return new Uint8Array([0, 255]); }
          finish();
          return null;
        } } };
      },
    });
  }

  test(`${kind}: only supported request body encodings are accepted at construction`, () => {
    for (const encoding of [null, "base64", "base64-v2", "", 1, {}])
      assert.throws(() => handler(encoding, assert.fail), /invalid requestBodyEncoding/);
  });

  for (const encoding of [undefined, "numeric-array", "base64-v1"]) {
    test(`${kind}: ${encoding ?? "default"} preserves the exact envelope and response bytes`, async () => {
      let received;
      const handle = handler(encoding, (input) => (received = input));
      const bodies = [
        new TextEncoder().encode("你好 🌍 café\u0000"),
        ...[0, 1, 2, 3, 256, 12287, 12288, 12289, 65534, 65535, 65536]
          .map((size) => Uint8Array.from({ length: size }, (_, i) => i % 256)),
      ];
      for (const bytes of bodies) {
        const request = new Request("https://proof.invalid/bytes?", {
          method: "POST", headers: [["x-test", "one"], ["x-test", "two"]], body: bytes,
        });
        const head = { method: "POST", uri: "/bytes?", headers: [...request.headers] };
        const response = await handle(request);
        assert.equal(response.status, 200);
        assert.deepEqual(new Uint8Array(await response.arrayBuffer()), new Uint8Array([0, 255]));
        const fields = encoding === "base64-v1"
          ? { body_encoding: "base64-v1", body_base64: Buffer.from(bytes).toString("base64") }
          : { body: [...bytes] };
        assert.equal(received, JSON.stringify({ ...head, ...fields }));
        if (encoding === "base64-v1")
          assert.deepEqual(Buffer.from(JSON.parse(received).body_base64, "base64"), Buffer.from(bytes));
      }
    });

    test(`${kind}: ${encoding ?? "default"} rejects excess body and head before encoding`, async (t) => {
      const handle = handler(encoding, () => assert.fail("rejected input entered Wasm"));
      const encode = t.mock.method(globalThis, "btoa");
      for (const [headers, size, status, error] of [
        [[], 65537, 413, "payload_too_large"],
        [[["content-length", "65537"]], 0, 413, "payload_too_large"],
        [[["x-large", "a".repeat(16384)]], 1, 431, "request_header_fields_too_large"],
      ]) {
        const response = await handle(new Request("https://proof.invalid/bytes", {
          method: "POST", headers, body: new Uint8Array(size),
        }));
        assert.equal(response.status, status);
        assert.deepEqual(await response.json(), { error });
        assert.equal(response.headers.get("cache-control"), "no-store");
        assert.equal(response.headers.get("x-content-type-options"), "nosniff");
      }
      assert.equal(encode.mock.callCount(), 0);
    });
  }
}
