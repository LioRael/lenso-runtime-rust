import test from "node:test";
import assert from "node:assert/strict";
import { createEventRunner } from "../runner.mjs";
import { createEventScope } from "../scope.mjs";
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((a, b) => {
    resolve = a;
    reject = b;
  });
  return { promise, resolve, reject };
};
const runner = (options) =>
  createEventRunner({
    instantiate: () => ({
      memory: new WebAssembly.Memory({ initial: 1 }),
      __wasm_call_ctors() {},
    }),
    clearTimers() {},
    resetState() {},
    ...options,
  });

test("headers do not retire a generation; clean terminal receipt closes its lease", async () => {
  const runtime = runner({ retirementAdmissionLimit: 1 }),
    terminal = deferred(),
    scope = createEventScope();
  const session = await runtime.open(
    () => ({ value: { status: 200 }, closed: terminal.promise }),
    { scope },
  );
  assert.equal(session.value.status, 200);
  assert.equal(runtime.generation(), 1);
  assert.equal(
    session.invoke(() => "next"),
    "next",
  );
  terminal.resolve({ shutdown: "clean" });
  await session.closed;
  assert.equal(runtime.generation(), 2);
  assert.throws(
    () => session.invoke(() => assert.fail("stale Wasm called")),
    /session_closed/,
  );
});

test("abandonment invalidates an open session and its late terminal receipt", async () => {
  const runtime = runner(),
    terminal = deferred();
  let cancelled = 0;
  const scope = createEventScope();
  scope.attach(() => cancelled++);
  const session = await runtime.open(
    () => ({ value: { status: 200 }, closed: terminal.promise }),
    { scope },
  );
  await assert.rejects(
    runtime.run(() => {
      throw Error("trap");
    }),
    /abandoned/,
  );
  await assert.rejects(session.closed, /abandoned/);
  assert.equal(
    cancelled,
    0,
    "foreign event never calls stale Rust cancellation",
  );
  assert.throws(() => session.invoke(() => assert.fail()), /session_closed/);
  terminal.resolve({ shutdown: "clean" });
  assert.equal((await runtime.run(() => "{}")).generation, 2);
});

test("open failure, non-clean terminal, session deadline and pre-abort are observable", async () => {
  const runtime = runner({ sessionLimitMs: 10 });
  await assert.rejects(
    runtime.open(() => Promise.reject(Error("startup"))),
    /abandoned/,
  );
  const bad = await runtime.open(() => ({
    value: {},
    closed: Promise.resolve({ shutdown: "unconfirmed" }),
  }));
  await assert.rejects(bad.closed, /session_shutdown_unconfirmed/);
  const stuck = await runtime.open(() => ({
    value: {},
    closed: new Promise(() => {}),
  }));
  await assert.rejects(stuck.closed, /session deadline/);
  await assert.rejects(
    runtime.open(() => assert.fail(), { signal: AbortSignal.abort() }),
    { name: "AbortError" },
  );
});
