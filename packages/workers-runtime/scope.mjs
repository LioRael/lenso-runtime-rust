/**
 * One request's native resources and detachable Wasm continuations.
 * Invalidation is synchronous JS-only fencing; abort/settled run in the owner.
 * Native adapters may track cleanup spawned by a late completion, but cannot
 * admit another application operation after this scope closes.
 */
export function createEventScope(
  bindings = {},
  { cleanupTimeoutMs = 250, maxOperations = 128 } = {},
) {
  if (!Number.isFinite(cleanupTimeoutMs) || cleanupTimeoutMs <= 0) {
    throw new RangeError("cleanupTimeoutMs must be positive and finite");
  }
  if (!Number.isSafeInteger(maxOperations) || maxOperations < 1) {
    throw new RangeError("maxOperations must be a positive integer");
  }
  let closed = false,
    invalidated = false,
    callback,
    cancelled = false;
  const pending = new Set(),
    gates = new Set(),
    aborters = new Set();
  let settlement;
  const closedError = () => new Error("event_scope_closed");

  function trackNative(value) {
    const promise = Promise.resolve(value);
    pending.add(promise);
    // Both handlers own rejection immediately and never call Wasm.
    promise.then(
      () => pending.delete(promise),
      () => pending.delete(promise),
    );
    return promise;
  }

  function operation(start, project = (value) => value) {
    if (closed) return { promise: Promise.reject(closedError()), abort() {} };
    if (aborters.size >= maxOperations) {
      return {
        promise: Promise.reject(new Error("event_operation_capacity")),
        abort() {},
      };
    }
    let resource;
    try {
      resource = start();
    } catch (error) {
      return { promise: Promise.reject(error), abort() {} };
    }
    if (!resource || !("promise" in resource))
      throw new TypeError("operation requires a promise");
    let aborted = false;
    const abort = () => {
      if (aborted) return;
      aborted = true;
      // Cancellation errors must be observed by bounded cleanup too.
      try {
        const result = resource.abort?.();
        if (result !== undefined) trackNative(result);
      } catch {
        cleanupFailed = true;
      }
    };
    aborters.add(abort);
    const gate = {};
    const promise = new Promise((resolve, reject) => {
      gate.resolve = resolve;
      gate.reject = reject;
    });
    gates.add(gate);
    const finish = (error, value) => {
      aborters.delete(abort);
      gates.delete(gate);
      const { resolve, reject } = gate;
      gate.resolve = gate.reject = undefined;
      if (invalidated) return;
      if (error) reject?.(value);
      else {
        try {
          resolve?.(project(value));
        } catch (failure) {
          reject?.(failure);
        }
      }
    };
    trackNative(resource.promise).then(
      (value) => finish(false, value),
      (error) => finish(true, error),
    );
    return { promise, abort };
  }

  let cleanupFailed = false;
  const scope = {
    get closed() {
      return closed;
    },
    get invalidated() {
      return invalidated;
    },
    trackNative,
    operation,
    run(start, project) {
      return operation(() => ({ promise: start() }), project).promise;
    },
    attach(next) {
      if (invalidated) return;
      callback = next;
      if (cancelled) callback?.();
    },
    detach() {
      callback = undefined;
    },
    invalidate() {
      closed = invalidated = true;
      callback = undefined;
      for (const gate of gates) gate.resolve = gate.reject = undefined;
      gates.clear();
    },
    abort() {
      closed = cancelled = true;
      const cancel = callback;
      callback = undefined;
      try {
        cancel?.();
      } catch {
        cleanupFailed = true;
      }
      for (const abort of [...aborters]) abort();
    },
    settled() {
      if (settlement) return settlement;
      // No admission once cleanup begins, even on an early HTTP rejection.
      closed = true;
      settlement = (async () => {
        let timer;
        const timeout = new Promise((resolve) => {
          timer = setTimeout(() => resolve(false), cleanupTimeoutMs);
        });
        const drain = async () => {
          // A completed read can start reader cancellation. A single snapshot
          // is insufficient: drain every newly registered native operation.
          while (pending.size) await Promise.allSettled([...pending]);
          return !cleanupFailed;
        };
        try {
          const clean = await Promise.race([drain(), timeout]);
          if (!clean) scope.invalidate();
          return clean;
        } finally {
          clearTimeout(timer);
        }
      })();
      return settlement;
    },
  };
  const values = typeof bindings === "function" ? bindings(scope) : bindings;
  if (!values || typeof values !== "object")
    throw new TypeError("event bindings must be an object");
  for (const name of Object.keys(values)) {
    if (name in scope || name === "bindings")
      throw new TypeError(`reserved event binding: ${name}`);
  }
  scope.bindings = Object.freeze({ ...values });
  // The direct projection preserves the existing wasm-bindgen Host ABI.
  Object.assign(scope, scope.bindings);
  return Object.freeze(scope);
}
