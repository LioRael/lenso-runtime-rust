import assert from "node:assert/strict";
import test from "node:test";
import { createEventScope } from "../scope.mjs";
import { createWorkersHttpHost } from "../host.mjs";

function generated(overrides = {}) {
  const calls = { init: [], constructors: 0, resets: 0 };
  const bindings = {
    initSync(options) {
      calls.init.push(options);
      return {
        memory: new WebAssembly.Memory({ initial: 1 }),
        __wasm_call_ctors() {
          calls.constructors++;
        },
      };
    },
    __wbg_reset_state() {
      calls.resets++;
    },
    handle_http(_input, scope) {
      return JSON.stringify({
        status: 200,
        headers: [],
        body: Array.from(new TextEncoder().encode(scope.bindings.requestId ?? "ok")),
        shutdown: "clean",
      });
    },
    ...overrides,
  };
  return { bindings, calls };
}

test("high-level Host wires generated module, flat limits, scopes, and receipt hooks", async () => {
  const { bindings, calls } = generated();
  const wasmModule = { name: "generated-bg" };
  const receipts = [];
  let clearCount = 0;
  const host = createWorkersHttpHost({
    bindings,
    wasmModule,
    limits: { eventLimitMs: 25, maxRequestBodyBytes: 4 },
    clearTimers: () => clearCount++,
    createScope(request, env, ctx) {
      assert.equal(env.name, "env");
      assert.equal(ctx.name, "ctx");
      return createEventScope({ requestId: request.headers.get("x-request") });
    },
    onReceipt(result, response, receiptRequest, receiptEnv, receiptCtx) {
      receipts.push(result);
      assert.equal(receiptRequest.url, "https://example.test/");
      assert.equal(receiptEnv.name, "env");
      assert.equal(receiptCtx.name, "ctx");
      response.headers.set("x-receipt", "seen");
    },
  });

  assert.deepEqual(calls.init, [{ module: wasmModule }]);
  assert.equal(calls.constructors, 1);
  const response = await host.fetch(
    new Request("https://example.test/", { headers: { "x-request": "A" } }),
    { name: "env" },
    { name: "ctx" },
  );
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("x-receipt"), "seen");
  assert.equal(new TextDecoder().decode(await response.arrayBuffer()), "A");
  assert.equal(receipts.length, 1);
  assert.equal(clearCount, 0);

  const oversized = await host.fetch(
    new Request("https://example.test/", { body: "12345", method: "POST" }),
    { name: "env" },
    { name: "ctx" },
  );
  assert.equal(oversized.status, 413);
});

test("scope factory receives isolated request env and execution context", async () => {
  const { bindings } = generated();
  const seen = [];
  const host = createWorkersHttpHost({
    bindings,
    wasmModule: {},
    createScope(request, env, ctx) {
      const scope = createEventScope({ requestId: env.id });
      seen.push({ request, env, ctx, scope });
      return scope;
    },
  });
  const [first, second] = await Promise.all([
    host.fetch(new Request("https://example.test/"), { id: "A" }, { id: 1 }),
    host.fetch(new Request("https://example.test/"), { id: "B" }, { id: 2 }),
  ]);
  assert.equal(new TextDecoder().decode(await first.arrayBuffer()), "A");
  assert.equal(new TextDecoder().decode(await second.arrayBuffer()), "B");
  assert.equal(seen.length, 2);
  assert.notEqual(seen[0].scope, seen[1].scope);
  assert.notEqual(seen[0].request, seen[1].request);
  assert.notEqual(seen[0].env, seen[1].env);
  assert.notEqual(seen[0].ctx, seen[1].ctx);
});

test("invalid generated modules and initialization failures fail closed", () => {
  assert.throws(
    () => createWorkersHttpHost({ bindings: {}, wasmModule: {} }),
    /bindings\.initSync/,
  );
  const { bindings } = generated();
  assert.throws(
    () => createWorkersHttpHost({ bindings }),
    /wasmModule is required/,
  );
  assert.throws(
    () => createWorkersHttpHost({ bindings, wasmModule: null }),
    /wasmModule is required/,
  );
  assert.throws(
    () => createWorkersHttpHost({ bindings, wasmModule: {}, createScope: () => createEventScope(), limits: { maxOperations: 2 } }),
    /configure scope limits/,
  );
  assert.throws(
    () =>
      createWorkersHttpHost({
        bindings: { ...bindings, __wbg_reset_state: undefined },
        wasmModule: {},
      }),
    /__wbg_reset_state/,
  );
  assert.throws(
    () => createWorkersHttpHost({ bindings, wasmModule: {}, limits: { eventLimtMs: 1 } }),
    /unknown limit: eventLimtMs/,
  );
  assert.throws(
    () => createWorkersHttpHost({ bindings, wasmModule: {}, createScope: () => createEventScope(), limits: { maxOperations: 2 } }),
    /configure scope limits/,
  );
  assert.throws(
    () =>
      createWorkersHttpHost({
        bindings: {
          ...bindings,
          initSync() {
            throw new Error("bad wasm");
          },
        },
        wasmModule: {},
      }),
    /bad wasm/,
  );
});

test("late completion remains fenced by the lower-level event scope", async () => {
  let finish;
  const native = new Promise((resolve) => {
    finish = resolve;
  });
  let callbacks = 0;
  const { bindings } = generated({
    handle_http(_input, scope) {
      scope.run(() => native).then(() => callbacks++);
      return new Promise(() => {});
    },
  });
  const host = createWorkersHttpHost({
    bindings,
    wasmModule: {},
    limits: { eventLimitMs: 10, cleanupTimeoutMs: 10 },
  });
  const response = await host.fetch(new Request("https://example.test/"));
  assert.equal(response.status, 503);
  finish("late");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(callbacks, 0);
});
