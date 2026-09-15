import { cleanupRelease } from "./cleanup.mjs";

// Product-neutral extraction of the qualified G1 generation boundary.
// Every run owns its continuation, cancellation scope and finalizer.
export function createEventRunner({
  instantiate,
  resetState,
  clearTimers,
  eventLimitMs = 1000,
  maxConcurrent = 32,
  retirementAdmissionLimit = 64,
  sessionLimitMs = 300_000,
  cancellationLimitMs = 1000,
}) {
  for (const [name, value] of Object.entries({
    eventLimitMs,
    sessionLimitMs,
    cancellationLimitMs,
  })) {
    if (!Number.isSafeInteger(value) || value < 1 || value > 0x7fffffff) {
      throw new RangeError(`${name} must be a positive 32-bit timer duration`);
    }
  }
  if (
    !Number.isInteger(maxConcurrent) ||
    maxConcurrent < 1 ||
    maxConcurrent > 32
  ) {
    throw new RangeError("maxConcurrent must be an integer from 1 to 32");
  }
  if (
    !Number.isInteger(retirementAdmissionLimit) ||
    retirementAdmissionLimit < 1 ||
    retirementAdmissionLimit > 96
  ) {
    throw new RangeError(
      "retirementAdmissionLimit must be an integer from 1 to 96",
    );
  }
  let exports = instantiate();
  exports.__wasm_call_ctors();
  let generation = 1;
  let unavailable = false;
  const pending = new Set();
  // One abandoned owner batch, independent of the initialized replacement.
  let quarantine;
  let initialized = true;
  function release(token) {
    quarantine?.delete(token);
    if (quarantine?.size === 0 && initialized) {
      quarantine = undefined;
      unavailable = false;
    }
  }

  // Bound retained allocation even if a dependency retains request App state.
  // Retire at the next idle boundary after the configured admission count; 96 is the hard ceiling.
  let admittedCount = 0;
  let queued = 0;
  function resetInstance() {
    unavailable = true;
    initialized = false;
    clearTimers();
    const previousMemory = exports.memory;
    resetState();
    exports = instantiate();
    if (exports.memory === previousMemory)
      throw new Error("Wasm memory was reused");
    exports.__wasm_call_ctors();
    admittedCount = 0;
    initialized = true;
    unavailable = !!quarantine;
  }
  function rotate() {
    generation++;
    resetInstance();
  }
  // Failure is distinct from clean retirement: reject every admitted event, then
  // rebuild. Owner-context finalizers dispose I/O after rejection.
  function abandon(cause) {
    if (quarantine || !initialized) return;
    quarantine = new Set([...pending].map((event) => event.cleanupToken));
    const abandoned = generation;
    const failure = new Error(
      `Wasm generation ${abandoned} abandoned: ${cause}`,
    );
    failure.code = "instance_abandoned";
    failure.generation = abandoned;
    unavailable = true;
    for (const event of pending) {
      clearTimeout(event.timer);
      event.invalidate();
      event.reject(failure);
    }
    pending.clear();
    try {
      rotate();
    } catch {
      /* Admission remains closed on reset failure. */
    }
  }

  function execute(operation, { scope, signal, prepare } = {}, opened) {
    if (signal?.aborted)
      return Promise.reject(new DOMException("Request aborted", "AbortError"));
    if (unavailable)
      return Promise.reject(new Error("Wasm instance unavailable"));
    if (admittedCount >= 96) {
      if (!pending.size) {
        try {
          rotate();
        } catch (error) {
          return Promise.reject(error);
        }
      } else {
        if (queued >= 32)
          return Promise.reject(new Error("Rotation queue capacity exceeded"));
        queued++;
        // Each waiter owns its timer. A shared cross-request Promise can be
        // canceled by workerd when its creating request has no local I/O left.
        return (async () => {
          try {
            const deadline = Date.now() + eventLimitMs;
            while (admittedCount >= 96 && pending.size) {
              if (signal?.aborted)
                throw new DOMException("Request aborted", "AbortError");
              if (unavailable) throw new Error("Wasm instance unavailable");
              if (Date.now() >= deadline)
                throw new Error("Rotation admission deadline exceeded");
              await new Promise((resolve) => setTimeout(resolve, 1));
            }
            return await execute(operation, { scope, signal, prepare }, opened);
          } finally {
            queued--;
          }
        })();
      }
    }
    if (pending.size >= maxConcurrent)
      return Promise.reject(new Error("Event capacity exceeded"));
    const admitted = generation;
    admittedCount++;
    let event, preparation;
    const cleanupToken = {};
    const preparationController = prepare ? new AbortController() : undefined;
    const result = new Promise((resolve, reject) => {
      const onAbort = () => {
        preparationController?.abort();
        scope?.abort();
        if (event?.started && !event.cancelling) {
          event.cancelling = true;
          clearTimeout(event.timer);
          event.timer = setTimeout(
            () => abandon("cancellation deadline exceeded"),
            cancellationLimitMs,
          );
        }
      };
      event = {
        cleanupToken,
        reject,
        invalidate() {
          scope?.invalidate?.();
        },
        dispose() {
          signal?.removeEventListener("abort", onAbort);
          preparationController?.abort();
          scope?.abort();
        },
      };
      signal?.addEventListener("abort", onAbort, { once: true });
      if (signal?.aborted) onAbort();
      pending.add(event);
      const start = (input) => {
        if (!pending.has(event) || admitted !== generation) return;
        if (signal?.aborted) {
          reject(new DOMException("Request aborted", "AbortError"));
          return;
        }
        event.started = true;
        event.timer = setTimeout(
          () => abandon("event deadline exceeded"),
          eventLimitMs,
        );
        let pendingOperation;
        try {
          pendingOperation = prepare ? operation(input) : operation();
        } catch (error) {
          abandon(String(error));
          return;
        }
        Promise.resolve(pendingOperation).then(
          (value) => {
            if (!pending.has(event) || admitted !== generation) return;
            if (opened) {
              // Headers may leave the Host now; the generation remains admitted until
              // the session's clean terminal receipt or a bounded failure.
              if (
                !value ||
                !value.closed ||
                typeof value.closed.then !== "function"
              ) {
                reject(
                  new TypeError("open operation must return { value, closed }"),
                );
                return;
              }
              // Late headers cannot extend the cancellation watchdog installed
              // at the first abort. Repeated cancellation keeps that deadline too.
              if (!event.cancelling) {
                clearTimeout(event.timer);
                event.timer = setTimeout(
                  () => abandon("session deadline exceeded"),
                  sessionLimitMs,
                );
              }
              opened({
                value: value.value,
                generation: admitted,
                invoke(call) {
                  if (
                    !pending.has(event) ||
                    admitted !== generation ||
                    scope?.closed
                  ) {
                    throw new Error("session_closed");
                  }
                  return call();
                },
                cancel() {
                  if (pending.has(event) && admitted === generation) onAbort();
                },
              });
              Promise.resolve(value.closed).then(
                (receipt) => {
                  if (!pending.has(event) || admitted !== generation) return;
                  clearTimeout(event.timer);
                  if (receipt?.shutdown !== "clean")
                    abandon("session_shutdown_unconfirmed");
                  else resolve(receipt);
                },
                (error) => {
                  if (admitted === generation) abandon(String(error));
                },
              );
            } else {
              clearTimeout(event.timer);
              try {
                resolve({
                  ...JSON.parse(value),
                  generation: admitted,
                  wasm_memory_bytes: exports.memory.buffer.byteLength,
                });
              } catch (error) {
                reject(error);
              }
            }
          },
          (error) => {
            if (admitted === generation) abandon(String(error));
          },
        );
      };
      // Capacity belongs to this request before transport preparation begins.
      // Its bounded body read has its own deadline; only Wasm entry starts the
      // event watchdog. Transport failures do not abandon healthy peers.
      if (prepare) {
        preparation = Promise.resolve().then(() => {
          if (!pending.has(event) || admitted !== generation) return;
          return prepare(preparationController.signal);
        });
        preparation.then(start, reject);
      } else start();
    });
    // Register cleanup in the owning fetch context. Another event may reject this
    // promise, but it must never directly abort this request's native I/O objects.
    return result.finally(async () => {
      let confirmed = false;
      try {
        clearTimeout(event?.timer);
        event?.dispose();
        // Abort and drain transport I/O here, in the owner continuation, even
        // when another request abandoned this generation during preparation.
        if (preparation) await preparation.catch(() => {});
        if ((await scope?.settled?.()) === false)
          throw new Error("Storage completion unavailable");
        confirmed = true;
      } catch {
        // Even a successful export must not retire past uncertain native I/O.
        if (admitted === generation) abandon("storage_cleanup_unconfirmed");
        // Only our scope can establish late release after its bounded timeout.
        // Unknown/rejected custom cleanup stays fail-closed until replacement.
        cleanupRelease.get(scope)?.then((released) => {
          if (released) release(cleanupToken);
        });
        const error = new Error("storage_cleanup_unconfirmed");
        error.status = 503;
        throw error;
      } finally {
        // Even a normal result can leave unconfirmed native work. Fence every
        // continuation before this event leaves the generation's pending set.
        event?.invalidate();
        pending.delete(event);
        if (confirmed) release(cleanupToken);
        if (
          !unavailable &&
          !pending.size &&
          admittedCount >= retirementAdmissionLimit
        )
          rotate();
      }
    });
  }
  function run(operation, options) {
    return execute(operation, options);
  }
  function open(operation, options) {
    let publish, fail;
    const ready = new Promise((resolve, reject) => {
      publish = resolve;
      fail = reject;
    });
    let session;
    const closed = execute(operation, options, (value) => {
      session = value;
      publish({ ...session, closed });
    });
    // Always own rejection even if a transport fails before consuming headers.
    // The returned closed Promise remains rejected for the owner to observe.
    closed.catch(fail);
    return ready;
  }
  return { run, open, generation: () => generation };
}
