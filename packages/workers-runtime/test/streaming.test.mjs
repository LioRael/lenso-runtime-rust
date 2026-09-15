import test from "node:test";
import assert from "node:assert/strict";
import { createStreamingHttpHandler } from "../http.mjs";
import { createEventRunner } from "../runner.mjs";
function fixture(read, options = {}) {
  let finish,
    reads = 0;
  const closed = new Promise((resolve) => (finish = resolve));
  const runner = createEventRunner({
    instantiate: () => ({
      __wasm_call_ctors() {},
      memory: { buffer: new ArrayBuffer(8) },
    }),
    resetState() {},
    clearTimers() {},
    eventLimitMs: 100,
    sessionLimitMs: 1000,
    cancellationLimitMs: 10,
    ...options,
  });
  const handler = createStreamingHttpHandler({
    open: runner.open,
    openHttp: (_input, scope) => {
      scope.attach(() => finish({ shutdown: "clean" }));
      return {
        value: {
          status: 200,
          headers: [["content-type", "application/octet-stream"]],
          read: async () => {
            reads++;
            const chunk = await read(reads);
            if (chunk === null) finish({ shutdown: "clean" });
            return chunk;
          },
        },
        closed,
      };
    },
  });
  return { handler, runner, reads: () => reads };
}
test("headers return without collecting or prefetching body; reads follow consumer demand", async () => {
  const f = fixture((n) => (n === 1 ? new Uint8Array([1, 2]) : null));
  const response = await f.handler(new Request("https://proof.invalid/"));
  assert.equal(f.reads(), 0);
  const reader = response.body.getReader();
  assert.deepEqual((await reader.read()).value, new Uint8Array([1, 2]));
  assert.equal(f.reads(), 1);
  assert.equal((await reader.read()).done, true);
});
test("consumer cancellation shuts down the event without reading remaining bytes", async () => {
  const f = fixture(() => new Uint8Array([1]));
  const response = await f.handler(new Request("https://proof.invalid/"));
  await response.body.cancel();
  assert.equal(f.reads(), 0);
});
test("generation abandonment errors an outstanding read instead of hanging or emitting clean EOF", async () => {
  const f = fixture(() => new Promise(() => {}));
  const response = await f.handler(new Request("https://proof.invalid/"));
  const pending = response.body.getReader().read();
  await assert.rejects(
    f.runner.run(() => {
      throw new Error("trap");
    }),
  );
  await assert.rejects(pending, /response_stream_failed/);
});
test("malformed stream chunks fail the response", async () => {
  const f = fixture(() => [1, 2]);
  const response = await f.handler(new Request("https://proof.invalid/"));
  await assert.rejects(
    response.body.getReader().read(),
    /response_stream_failed/,
  );
});
