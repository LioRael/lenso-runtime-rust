import assert from "node:assert/strict";
import { test } from "node:test";
import { createHttpHandler } from "../http.mjs";
import { transportAdmissionTests } from "./transport-admission.mjs";

transportAdmissionTests(createHttpHandler, false);
transportAdmissionTests(createHttpHandler, false, "base64-v1");

test("binary response encoding preserves bytes and enforces decoded size and canonical shape", async () => {
  async function handle(payload, limit = 4) {
    return createHttpHandler({
      run: async (operation) => operation(),
      maxResponseBodyBytes: limit,
      handleHttp: () => ({
        status: 200,
        headers: [],
        shutdown: "clean",
        ...payload,
      }),
    })(new Request("https://proof.invalid/binary"));
  }
  for (const payload of [
    { body_base64: "AP+AQQ==" },
    { body: [0, 255, 128, 65] },
  ]) {
    const response = await handle(payload);
    assert.equal(response.status, 200);
    assert.deepEqual(
      [...new Uint8Array(await response.arrayBuffer())],
      [0, 255, 128, 65],
    );
  }
  for (const encoded of ["AAAAAA==", "AAAA"]) {
    const response = await handle({ body_base64: encoded }, 2);
    assert.equal(response.status, 502);
    assert.deepEqual(await response.json(), {
      error: "response_body_too_large",
    });
  }
  for (const payload of [
    { body_base64: "AB==" },
    { body_base64: "AA" },
    { body_base64: "AA-_" },
    { body_base64: "AA==\n" },
    { body_base64: null },
    { body_base64: "", body: [] },
    { body: [256] },
  ]) {
    const response = await handle(payload);
    assert.equal(response.status, 502);
    assert.deepEqual(await response.json(), { error: "invalid_response_body" });
  }
  assert.equal((await handle({ body_base64: "" }, 0)).status, 200);
  const large = new Uint8Array(3_400_000).fill(255);
  const response = await handle(
    { body_base64: Buffer.from(large).toString("base64") },
    4 * 1024 * 1024,
  );
  assert.equal(response.status, 200);
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), large);
});

test("missing event bindings reject before admission with a normalized response", async () => {
  const handler = createHttpHandler({
    createScope() {
      throw new Error("Required event binding is unavailable");
    },
    run() {
      assert.fail("Kernel must not start without its required event binding");
    },
    handleHttp() {
      assert.fail("HTTP must not enter the Plugin before Ready");
    },
  });
  const response = await handler(new Request("https://proof.invalid/method"));
  assert.equal(response.status, 503);
  assert.equal(response.headers.get("x-content-type-options"), "nosniff");
  assert.deepEqual(await response.json(), { error: "host_unavailable" });
});

test("held storage work reports cleanup uncertainty after generation deadline without replay", async () => {
  const { createEventRunner } = await import("../runner.mjs");
  let attempts = 0;
  let invalidated = false;
  let aborts = 0;
  const runner = createEventRunner({
    instantiate: () => ({
      memory: new WebAssembly.Memory({ initial: 1 }),
      __wasm_call_ctors() {},
    }),
    resetState() {},
    clearTimers() {},
    eventLimitMs: 10,
  });
  const handler = createHttpHandler({
    run: runner.run,
    handleHttp() {
      attempts++;
      return new Promise(() => {});
    },
    createScope() {
      return {
        abort() {
          aborts++;
        },
        invalidate() {
          invalidated = true;
        },
        // The storage adapter owns this finite settlement limit.
        settled: () =>
          new Promise((resolve) => setTimeout(() => resolve(false), 5)),
      };
    },
  });
  const response = await handler(new Request("https://proof.invalid/write"));
  assert.equal(response.status, 503);
  assert.deepEqual(await response.json(), {
    error: "storage_cleanup_unconfirmed",
  });
  assert.equal(attempts, 1);
  assert.equal(invalidated, true);
  assert.ok(aborts > 0);
});

test("storage settlement errors normalize instead of escaping the fetch handler", async () => {
  const handler = createHttpHandler({
    run: async (operation) => operation(),
    handleHttp: () => ({
      status: 200,
      headers: [],
      body: [],
      shutdown: "clean",
    }),
    createScope: () => ({
      abort() {},
      settled() {
        throw new Error("storage completion unavailable");
      },
    }),
  });
  const response = await handler(new Request("https://proof.invalid/read"));
  assert.equal(response.status, 503);
  assert.deepEqual(await response.json(), {
    error: "storage_cleanup_unconfirmed",
  });
});

test("consumer admission limit and retirement retain the fixed generation ceiling", async () => {
  const { createEventRunner } = await import("../runner.mjs");
  const options = {
    instantiate: () => ({
      memory: new WebAssembly.Memory({ initial: 1 }),
      __wasm_call_ctors() {},
    }),
    resetState() {},
    clearTimers() {},
  };
  for (const maxConcurrent of [0, 33, 1.5])
    assert.throws(
      () => createEventRunner({ ...options, maxConcurrent }),
      RangeError,
    );
  for (const retirementAdmissionLimit of [0, 97, 1.5])
    assert.throws(
      () => createEventRunner({ ...options, retirementAdmissionLimit }),
      RangeError,
    );
  const runner = createEventRunner({
    ...options,
    maxConcurrent: 1,
    retirementAdmissionLimit: 1,
  });
  let finish;
  const first = runner.run(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  await assert.rejects(
    runner.run(() => "{}"),
    /Event capacity exceeded/,
  );
  finish("{}");
  await first;
  assert.equal(runner.generation(), 2);
});

test("normal operation with unconfirmed storage fences late callbacks before retirement", async () => {
  const { createEventRunner } = await import("../runner.mjs");
  let live = true;
  let calls = 0;
  let finishNative;
  const native = new Promise((resolve) => {
    finishNative = resolve;
  });
  void native.then(() => {
    if (live) calls++;
  });
  const runner = createEventRunner({
    instantiate: () => ({
      memory: new WebAssembly.Memory({ initial: 1 }),
      __wasm_call_ctors() {},
    }),
    resetState() {
      assert.equal(live, false, "Old callbacks must be fenced before reset");
    },
    clearTimers() {},
    retirementAdmissionLimit: 1,
  });
  await assert.rejects(
    runner.run(() => "{}", {
      scope: {
        abort() {},
        settled: async () => false,
        invalidate() {
          live = false;
        },
      },
    }),
    { message: "storage_cleanup_unconfirmed", status: 503 },
  );
  finishNative();
  await native;
  assert.equal(calls, 0);
  assert.equal(runner.generation(), 2);
});

test("early body length rejection releases the incoming stream without Kernel admission", async () => {
  let cancelled = false;
  const body = new ReadableStream({
    cancel() {
      cancelled = true;
    },
  });
  const handler = createHttpHandler({
    maxRequestBodyBytes: 4,
    run() {
      assert.fail("Oversized input must not enter Kernel");
    },
    handleHttp() {
      assert.fail("Oversized input must not enter Plugin");
    },
  });
  const response = await handler(
    new Request("https://proof.invalid/write", {
      method: "POST",
      headers: { "content-length": "5" },
      body,
      duplex: "half",
    }),
  );
  assert.equal(response.status, 413);
  assert.equal(cancelled, true);
});
