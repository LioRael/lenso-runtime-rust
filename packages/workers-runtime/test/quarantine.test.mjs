import test from "node:test";
import assert from "node:assert/strict";
import { createEventRunner } from "../runner.mjs";
import { createEventScope } from "../scope.mjs";

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
};
const turn = () => new Promise((resolve) => setImmediate(resolve));
function fixture(options = {}) {
  let resets = 0;
  const runtime = createEventRunner({
    instantiate: () => ({
      memory: new WebAssembly.Memory({ initial: 1 }),
      __wasm_call_ctors() {},
    }),
    resetState() {
      resets++;
    },
    clearTimers() {},
    ...options,
  });
  return { runtime, resets: () => resets };
}

for (const outcome of ["resolve", "reject"])
  test(`abandoned owner cleanup ${outcome}: successor cannot prepare or enter Wasm`, async () => {
    const { runtime, resets } = fixture();
    const cleanup = deferred(),
      cleaning = deferred(),
      terminal = deferred();
    let invalidated = false;
    const session = await runtime.open(
      () => ({ value: {}, closed: terminal.promise }),
      {
        scope: {
          invalidate() {
            invalidated = true;
          },
          abort() {},
          settled() {
            cleaning.resolve();
            return cleanup.promise;
          },
        },
      },
    );
    const closed = assert.rejects(
      session.closed,
      outcome === "resolve" ? /abandoned/ : /storage_cleanup_unconfirmed/,
    );
    const fault = runtime.run(() => {
      throw Error("trap");
    });
    assert.equal(invalidated, true);
    assert.equal(
      resets(),
      1,
      "guards reset synchronously before any owner continuation",
    );
    await assert.rejects(fault, /abandoned/);
    await cleaning.promise;
    await assert.rejects(
      runtime.run(() => assert.fail("successor Wasm"), {
        prepare() {
          assert.fail("successor body");
        },
      }),
      /unavailable/,
    );
    assert.throws(
      () => session.invoke(() => assert.fail("stale callback")),
      /session_closed/,
    );
    assert.equal(
      (await fixture().runtime.run(() => "{}")).generation,
      1,
      "unrelated domain stays healthy",
    );
    cleanup[outcome](outcome === "resolve" ? true : Error("cleanup rejected"));
    await closed;
    terminal.resolve({ shutdown: "clean" });
    await turn();
    if (outcome === "resolve")
      assert.equal((await runtime.run(() => "{}")).generation, 2);
    else
      await assert.rejects(
        runtime.run(() => assert.fail("unknown release")),
        /unavailable/,
      );
    assert.equal(resets(), 1);
  });

for (const outcome of ["resolve", "reject"])
  test(`timeout receipt stays uncertain; late native ${outcome} releases quarantine without Wasm delivery`, async () => {
    const { runtime, resets } = fixture();
    const scope = createEventScope({}, { cleanupTimeoutMs: 10 });
    const native = deferred();
    let deliveries = 0;
    scope
      .run(() => native.promise)
      .then(
        () => deliveries++,
        () => deliveries++,
      );
    await assert.rejects(
      runtime.run(() => "{}", { scope }),
      /storage_cleanup_unconfirmed/,
    );
    for (let index = 0; index < 3; index++)
      await assert.rejects(
        runtime.run(() => assert.fail("quarantined")),
        /unavailable/,
      );
    assert.equal(resets(), 1);
    native[outcome]("late native completion");
    await turn();
    assert.equal(await scope.settled(), false, "failed receipt is sticky");
    assert.equal(deliveries, 0);
    assert.equal((await runtime.run(() => "{}")).generation, 2);
  });

test("all owners must release; no reset loop behind never-settling cleanup", async () => {
  const { runtime, resets } = fixture();
  const a = deferred(),
    b = deferred();
  const owner = (cleanup) =>
    runtime.run(() => new Promise(() => {}), {
      scope: { abort() {}, invalidate() {}, settled: () => cleanup.promise },
    });
  const first = assert.rejects(owner(a), /abandoned/);
  const second = assert.rejects(owner(b), /abandoned/);
  await assert.rejects(
    runtime.run(() => {
      throw Error("trap");
    }),
    /abandoned/,
  );
  a.resolve(true);
  await first;
  await assert.rejects(
    runtime.run(() => assert.fail()),
    /unavailable/,
  );
  assert.equal(resets(), 1);
  b.resolve(true);
  await second;
  assert.equal((await runtime.run(() => "{}")).generation, 2);
});

for (const failure of ["reset", "memory", "constructors"])
  test(`${failure} failure remains unavailable after cleanup`, async () => {
    let instances = 0;
    const memory = new WebAssembly.Memory({ initial: 1 });
    const { runtime } = fixture({
      instantiate() {
        instances++;
        return {
          memory:
            failure === "memory"
              ? memory
              : new WebAssembly.Memory({ initial: 1 }),
          __wasm_call_ctors() {
            if (instances > 1 && failure === "constructors")
              throw Error("constructors");
          },
        };
      },
      resetState() {
        if (failure === "reset") throw Error("reset");
      },
    });
    await assert.rejects(
      runtime.run(() => {
        throw Error("trap");
      }),
      /abandoned/,
    );
    await assert.rejects(
      runtime.run(() => assert.fail()),
      /unavailable/,
    );
  });

test("late headers and repeated cancellation keep the original cancellation deadline", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const { runtime } = fixture({ cancellationLimitMs: 100 });
  const headers = deferred(),
    terminal = deferred();
  const controller = new AbortController();
  const ready = runtime.open(() => headers.promise, {
    signal: controller.signal,
  });
  controller.abort();
  t.mock.timers.tick(90);
  headers.resolve({ value: {}, closed: terminal.promise });
  const session = await ready;
  const closed = assert.rejects(session.closed, /cancellation deadline/);
  session.cancel();
  t.mock.timers.tick(10);
  await closed;
  assert.equal(runtime.generation(), 2);
  terminal.resolve({ shutdown: "clean" });
});

test("rejected native abort cleanup is observed and remains fail-closed", async () => {
  const { runtime } = fixture();
  const scope = createEventScope();
  const native = deferred(),
    cleaning = deferred();
  scope.operation(() => ({
    promise: native.promise,
    abort() {
      cleaning.resolve();
      return Promise.reject(Error("abort failed"));
    },
  }));
  const work = runtime.run(() => "{}", { scope });
  await cleaning.promise;
  native.resolve();
  await assert.rejects(work, /storage_cleanup_unconfirmed/);
  await assert.rejects(
    runtime.run(() => assert.fail("unconfirmed abort release")),
    /unavailable/,
  );
});
