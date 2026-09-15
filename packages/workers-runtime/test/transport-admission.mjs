import assert from "node:assert/strict";
import test from "node:test";
import { createEventRunner } from "../runner.mjs";
import { createEventScope } from "../scope.mjs";

function deferred() {
  let resolve;
  const promise = new Promise((finish) => (resolve = finish));
  return { promise, resolve };
}

function incoming({ pull, cancel } = {}) {
  let reads = 0, cancels = 0, readers = 0;
  const body = new ReadableStream({
    pull(controller) {
      reads++;
      if (pull) return pull(controller);
      controller.enqueue(new Uint8Array([65]));
      controller.close();
    },
    cancel() {
      cancels++;
      return cancel?.();
    },
  }, { highWaterMark: 0 });
  const getReader = body.getReader.bind(body);
  body.getReader = (...args) => {
    readers++;
    return getReader(...args);
  };
  return {
    request: (options = {}) => new Request("https://proof.invalid/admission", {
      method: "POST", body, duplex: "half", ...options,
    }),
    counts: () => ({ reads, cancels, readers }),
    body,
  };
}

export function transportAdmissionTests(createHandler, streaming) {
  const kind = streaming ? "streaming" : "buffered";
  function fixture({ runnerOptions, handlerOptions, handle } = {}) {
    const inputs = [];
    const runner = createEventRunner({
      instantiate: () => ({
        memory: new WebAssembly.Memory({ initial: 1 }),
        __wasm_call_ctors() {},
      }),
      resetState() {},
      clearTimers() {},
      maxConcurrent: 1,
      ...runnerOptions,
    });
    const handler = createHandler({
      run: runner.run,
      open: runner.open,
      handleHttp: invoke,
      openHttp: invoke,
      ...handlerOptions,
    });
    function invoke(input, scope) {
      inputs.push(JSON.parse(input));
      if (handle) return handle(input, scope);
      if (!streaming)
        return JSON.stringify({ status: 200, headers: [], body: [65], shutdown: "clean" });
      const terminal = deferred();
      scope.attach(() => terminal.resolve({ shutdown: "clean" }));
      let read = false;
      return {
        value: {
          status: 200, headers: [],
          read() {
            if (!read) {
              read = true;
              return new Uint8Array([65]);
            }
            terminal.resolve({ shutdown: "clean" });
            return null;
          },
        },
        closed: terminal.promise,
      };
    }
    return { runner, handler, inputs };
  }

  async function recover(f) {
    const body = incoming();
    const response = await f.handler(body.request());
    assert.equal(response.status, 200);
    assert.equal(await response.text(), "A");
    assert.deepEqual(f.inputs.at(-1).body, [65]);
    // A completed reservation cannot remove a later active event's slot.
    const terminal = deferred();
    const active = f.runner.run(() => terminal.promise);
    await assert.rejects(f.runner.run(() => assert.fail("over-admission")), /capacity/);
    terminal.resolve("{}");
    await active;
  }

  test(`${kind}: overload rejects before acquiring a body reader or invoking Wasm`, async () => {
    const f = fixture();
    const terminal = deferred();
    const active = f.runner.run(() => terminal.promise);
    try {
      const body = incoming();
      const response = await f.handler(body.request());
      assert.equal(response.status, 503);
      assert.deepEqual(await response.json(), { error: "host_unavailable" });
      assert.deepEqual(body.counts(), { reads: 0, readers: 0, cancels: 1 });
      assert.equal(f.inputs.length, 0);
    } finally {
      terminal.resolve("{}");
      await active;
    }
    await recover(f);
  });

  test(`${kind}: an in-flight body read reserves capacity and retains its own deadline`, async () => {
    const started = deferred(), release = deferred();
    const f = fixture({ runnerOptions: { eventLimitMs: 10 } });
    const body = incoming({ async pull(controller) {
      started.resolve();
      await release.promise;
      controller.close();
    } });
    const first = f.handler(body.request());
    await started.promise;
    try {
      const second = incoming();
      assert.equal((await f.handler(second.request())).status, 503);
      assert.deepEqual(second.counts(), { reads: 0, readers: 0, cancels: 1 });
      // Transport preparation must not use the shorter Wasm event watchdog.
      await new Promise((resolve) => setTimeout(resolve, 25));
      assert.equal(f.runner.generation(), 1);
      assert.equal(f.inputs.length, 0);
    } finally {
      release.resolve();
    }
    const response = await first;
    assert.equal(response.status, 200);
    await response.arrayBuffer();
    await recover(f);
  });

  for (const failure of ["head", "length", "limit", "read", "timeout", "abort", "pre-abort"]) {
    test(`${kind}: ${failure} transport failure releases capacity without abandoning the generation`, async () => {
      const started = deferred();
      const controller = new AbortController();
      const body = incoming({ pull(stream) {
        started.resolve();
        if (failure === "read") stream.error(new Error("incoming read failed"));
        else if (!["timeout", "abort"].includes(failure)) {
          stream.enqueue(new Uint8Array([1, 2]));
          if (failure !== "limit") stream.close();
        }
      } });
      const f = fixture({ handlerOptions: {
        maxRequestBodyBytes: 1,
        maxRequestHeadBytes: failure === "head" ? 1 : 16384,
        bodyReadTimeoutMs: 10,
      } });
      if (failure === "pre-abort") controller.abort();
      const responsePromise = f.handler(body.request({
        signal: controller.signal,
        ...(failure === "length" ? { headers: { "content-length": "2" } } : {}),
      }));
      if (failure === "abort") {
        await started.promise;
        controller.abort();
      }
      const response = await responsePromise;
      const status = { head: 431, length: 413, limit: 413, timeout: 408 }[failure] ?? 503;
      assert.equal(response.status, status);
      assert.equal(f.runner.generation(), 1);
      assert.equal(f.inputs.length, 0);
      assert.equal(body.body.locked, false);
      if (failure !== "read") assert.equal(body.counts().cancels, 1);
      // Recover through the same runner with normal transport limits.
      const terminal = deferred();
      const active = f.runner.run(() => terminal.promise);
      await assert.rejects(f.runner.run(() => assert.fail()), /capacity/);
      terminal.resolve("{}");
      await active;
    });
  }

  test(`${kind}: handler failure abandons once and later requests recover`, async () => {
    let fail = true;
    const f = fixture({ handle() {
      if (fail) {
        fail = false;
        throw new Error("Wasm trap");
      }
      return streaming
        ? { value: { status: 204, headers: [], read() {} }, closed: Promise.resolve({ shutdown: "clean" }) }
        : JSON.stringify({ status: 204, headers: [], body: [], shutdown: "clean" });
    } });
    assert.equal((await f.handler(incoming().request())).status, 503);
    assert.equal(f.runner.generation(), 2);
    assert.equal((await f.handler(incoming().request())).status, 204);
    assert.equal(f.inputs.length, 2);
    assert.equal(f.runner.generation(), 2);
  });

  test(`${kind}: stalled incoming cancellation settles within the scope budget`, async () => {
    const started = deferred(), cancelled = deferred();
    const controller = new AbortController();
    const f = fixture({ handlerOptions: {
      createScope: () => createEventScope({}, { cleanupTimeoutMs: 10 }),
    } });
    const body = incoming({
      pull() { started.resolve(); },
      cancel: () => cancelled.promise,
    });
    const pending = f.handler(body.request({ signal: controller.signal }));
    await started.promise;
    controller.abort();
    const response = await pending;
    assert.equal(response.status, 503);
    assert.deepEqual(await response.json(), { error: "storage_cleanup_unconfirmed" });
    assert.equal(body.body.locked, false);
    assert.equal(body.counts().cancels, 1);
    assert.equal(f.inputs.length, 0);
    cancelled.resolve();
    await recover(f);
  });

  test(`${kind}: cleanup retains capacity and uncertain storage fails closed`, async () => {
    const settling = deferred(), settled = deferred();
    const f = fixture({ handlerOptions: {
      createScope() {
        const scope = createEventScope();
        return { ...scope, settled() {
          settling.resolve();
          return settled.promise;
        } };
      },
    } });
    const first = f.handler(new Request("https://proof.invalid/"));
    // Streaming cleanup follows session close, after headers have left.
    const response = streaming ? await first : undefined;
    const consumed = response?.arrayBuffer();
    if (consumed) consumed.catch(() => {});
    await settling.promise;
    await assert.rejects(f.runner.run(() => assert.fail()), /capacity/);
    settled.resolve(false);
    if (streaming) await assert.rejects(consumed, /response_stream_failed/);
    else assert.deepEqual(await (await first).json(), { error: "storage_cleanup_unconfirmed" });
    assert.equal((await f.runner.run(() => "{}")).generation, 1);
  });

  test(`${kind}: foreign abandonment cancels preparation in its owner and fences Wasm entry`, async () => {
    const started = deferred();
    let foreign = false;
    const body = incoming({
      pull() { started.resolve(); },
      cancel() { assert.equal(foreign, false, "foreign request touched incoming I/O"); },
    });
    const f = fixture({ runnerOptions: { maxConcurrent: 2 } });
    const first = f.handler(body.request());
    await started.promise;
    foreign = true;
    const failed = f.runner.run(() => { throw new Error("peer trap"); });
    foreign = false;
    await assert.rejects(failed, /abandoned/);
    assert.equal((await first).status, 503);
    assert.deepEqual(body.counts(), { reads: 1, readers: 1, cancels: 1 });
    assert.equal(body.body.locked, false);
    assert.equal(f.inputs.length, 0);
    assert.equal(f.runner.generation(), 2);
    const response = await f.handler(incoming().request());
    assert.equal(response.status, 200);
    await response.arrayBuffer();
  });

  if (streaming) test("streaming: response cancellation releases its reservation only after session close", async () => {
    const f = fixture();
    const first = await f.handler(incoming().request());
    const overloaded = incoming();
    assert.equal((await f.handler(overloaded.request())).status, 503);
    assert.deepEqual(overloaded.counts(), { reads: 0, readers: 0, cancels: 1 });
    await first.body.cancel();
    await recover(f);
  });
}
