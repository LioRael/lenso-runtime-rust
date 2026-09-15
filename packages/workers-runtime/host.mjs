import { clearTimers as defaultClearTimers } from "./clock.mjs";
import { createHttpHandler } from "./http.mjs";
import { createEventRunner } from "./runner.mjs";
import { createEventScope } from "./scope.mjs";
import { validateRequestBodyEncoding } from "./request-body.mjs";

const RUNNER_LIMITS = [
  "eventLimitMs",
  "maxConcurrent",
  "retirementAdmissionLimit",
  "sessionLimitMs",
  "cancellationLimitMs",
];
const HTTP_LIMITS = [
  "maxRequestBodyBytes",
  "maxResponseBodyBytes",
  "maxRequestHeadBytes",
  "bodyReadTimeoutMs",
];
const SCOPE_LIMITS = ["cleanupTimeoutMs", "maxOperations"];
const LIMIT_NAMES = new Set([
  ...RUNNER_LIMITS,
  ...HTTP_LIMITS,
  ...SCOPE_LIMITS,
]);

function requireObject(value, name) {
  if (!value || typeof value !== "object")
    throw new TypeError(`${name} must be an object`);
  return value;
}

function pickLimits(limits, names) {
  const selected = {};
  for (const name of names) {
    if (name in limits) selected[name] = limits[name];
  }
  return selected;
}

function validateLimits(limits) {
  requireObject(limits, "limits");
  if (Array.isArray(limits)) throw new TypeError("limits must be an object");
  for (const name of Object.keys(limits)) {
    if (!LIMIT_NAMES.has(name)) throw new TypeError(`unknown limit: ${name}`);
  }
}

function generatedInstantiate(bindings, wasmModule) {
  return () => {
    const exports = bindings.initSync({ module: wasmModule });
    if (!exports || typeof exports !== "object")
      throw new TypeError("generated initSync must return a module namespace");
    // wasm-bindgen normally places the constructor on the returned namespace.
    // Accept a namespace export too so generated compatibility shims use the
    // same Host entry without exposing lifecycle wiring to each consumer.
    if (typeof exports.__wasm_call_ctors !== "function") {
      if (typeof bindings.__wasm_call_ctors !== "function")
        throw new TypeError("generated module must export __wasm_call_ctors");
      return { ...exports, __wasm_call_ctors: bindings.__wasm_call_ctors };
    }
    return exports;
  };
}

/**
 * Assemble the stable buffered Workers HTTP Host boundary.
 *
 * `bindings` is the complete generated wasm-bindgen module namespace and
 * `wasmModule` is its corresponding `*_bg.wasm` module. The scope factory is
 * the explicit composition seam for Plugin and resource bindings; it receives
 * the Cloudflare request, env, and execution context for every request.
 */
export function createWorkersHttpHost({
  bindings,
  wasmModule,
  requestBodyEncoding = "numeric-array",
  limits = {},
  createScope,
  onReceipt,
  clearTimers = defaultClearTimers,
} = {}) {
  validateRequestBodyEncoding(requestBodyEncoding);
  requireObject(bindings, "bindings");
  if (typeof bindings.initSync !== "function")
    throw new TypeError("bindings.initSync must be a function");
  if (typeof bindings.__wbg_reset_state !== "function")
    throw new TypeError("bindings.__wbg_reset_state must be a function");
  if (wasmModule == null) throw new TypeError("wasmModule is required");
  if (typeof clearTimers !== "function")
    throw new TypeError("clearTimers must be a function");
  const handleHttp = bindings.handle_http;
  if (typeof handleHttp !== "function")
    throw new TypeError("bindings.handle_http must be a function");
  validateLimits(limits);
  if (createScope !== undefined && typeof createScope !== "function")
    throw new TypeError("createScope must be a function");
  if (createScope && SCOPE_LIMITS.some((name) => name in limits))
    throw new TypeError("configure scope limits inside the explicit createScope factory");
  if (onReceipt !== undefined && typeof onReceipt !== "function")
    throw new TypeError("onReceipt must be a function");

  const runner = createEventRunner({
    instantiate: generatedInstantiate(bindings, wasmModule),
    resetState: bindings.__wbg_reset_state,
    clearTimers,
    ...pickLimits(limits, RUNNER_LIMITS),
  });
  const scopeLimits = pickLimits(limits, SCOPE_LIMITS);
  const makeScope =
    createScope ?? (() => createEventScope({}, scopeLimits));
  const httpLimits = pickLimits(limits, HTTP_LIMITS);

  // Keep the handler construction inside fetch so env/ctx are request-owned.
  // The runner remains shared by the isolate, as required by generation reset.
  async function fetch(request, env, ctx) {
    const handler = createHttpHandler({
      ...httpLimits,
      requestBodyEncoding,
      run: runner.run,
      handleHttp,
      createScope: () => makeScope(request, env, ctx),
      ...(onReceipt === undefined
        ? {}
        : {
            onReceipt: (receipt, response) =>
              onReceipt(receipt, response, request, env, ctx),
          }),
    });
    return handler(request);
  }

  return Object.freeze({ fetch });
}
