import assert from "node:assert/strict";
import test from "node:test";
import { createEventScope } from "../scope.mjs";

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
};
const tick = () => new Promise((resolve) => setImmediate(resolve));

for (const outcome of ["resolve", "reject"]) {
  test(`generation invalidation fences late ${outcome} without invoking foreign I/O`, async () => {
    const scope = createEventScope();
    const native = deferred();
    let callbacks = 0,
      aborts = 0,
      rustCancels = 0;
    scope.attach(() => rustCancels++);
    scope
      .operation(() => ({ promise: native.promise, abort: () => aborts++ }))
      .promise.then(
        () => callbacks++,
        () => callbacks++,
      );
    scope.invalidate();
    assert.equal(aborts, 0);
    assert.equal(rustCancels, 0);
    scope.abort();
    assert.equal(aborts, 1);
    native[outcome]("late");
    assert.equal(await scope.settled(), true);
    await tick();
    assert.equal(callbacks, 0);
  });
}

test("normal result and domain rejection reach the current event", async () => {
  const scope = createEventScope();
  assert.equal(
    await scope.run(
      () => Promise.resolve(3),
      (value) => value + 1,
    ),
    4,
  );
  const failure = new Error("native failure");
  await assert.rejects(
    scope.run(() => Promise.reject(failure)),
    (error) => error === failure,
  );
  await assert.rejects(
    scope.run(() => {
      throw failure;
    }),
    (error) => error === failure,
  );
  scope.abort();
  assert.equal(await scope.settled(), true);
});

test("scope owns every operation without adapter lifecycle registration", async () => {
  const first = deferred(),
    second = deferred();
  const scope = createEventScope();
  let aborts = 0;
  const start = (native) =>
    scope.operation(() => ({
      promise: native.promise,
      abort() {
        aborts++;
        native.resolve("closed");
      },
    })).promise;
  const values = [start(first), start(second)];
  scope.abort();
  scope.abort();
  assert.equal(aborts, 2);
  assert.equal(await scope.settled(), true);
  assert.deepEqual(await Promise.all(values), ["closed", "closed"]);
});

test("cleanup includes cancellation created after the first pending snapshot", async () => {
  const read = deferred(),
    cancel = deferred();
  const scope = createEventScope({}, { cleanupTimeoutMs: 200 });
  const tracked = scope.trackNative(read.promise);
  tracked.then(() => scope.trackNative(cancel.promise));
  let finished = false;
  const settlement = scope.settled().then((value) => {
    finished = true;
    return value;
  });
  read.resolve();
  await tick();
  assert.equal(finished, false);
  cancel.resolve();
  assert.equal(await settlement, true);
});

test("unabortable work has bounded uncertain cleanup and detached callbacks", async () => {
  const scope = createEventScope({}, { cleanupTimeoutMs: 10 });
  const native = deferred();
  let callbacks = 0;
  scope.run(() => native.promise).then(() => callbacks++);
  scope.abort();
  const settlement = scope.settled();
  assert.equal(scope.settled(), settlement);
  assert.equal(await settlement, false);
  native.resolve("committed");
  await tick();
  assert.equal(callbacks, 0);
});

test("capacity and closed admission cannot start native side effects", async () => {
  const scope = createEventScope({}, { maxOperations: 1 });
  const native = deferred();
  const first = scope.run(() => native.promise);
  let starts = 0;
  await assert.rejects(
    scope.run(() => {
      starts++;
    }),
    /capacity/,
  );
  scope.abort();
  await assert.rejects(
    scope.run(() => {
      starts++;
    }),
    /closed/,
  );
  assert.equal(starts, 0);
  native.resolve();
  await first;
  assert.equal(await scope.settled(), true);
});

test("abort failures are uncertain cleanup, never a successful receipt", async () => {
  const scope = createEventScope();
  const native = deferred();
  scope.operation(() => ({
    promise: native.promise,
    abort() {
      throw new Error("cannot abort");
    },
  }));
  scope.abort();
  native.resolve();
  assert.equal(await scope.settled(), false);
});

test("bindings cannot override lifecycle methods and are immutable", () => {
  assert.throws(() => createEventScope({ invalidate() {} }), /reserved/);
  const scope = createEventScope((owner) => ({
    query: () => owner.run(() => Promise.resolve("ok")),
  }));
  assert.equal(scope.query, scope.bindings.query);
  assert.equal(Object.isFrozen(scope), true);
  assert.equal(Object.isFrozen(scope.bindings), true);
});
