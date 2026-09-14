// Product-neutral extraction of the qualified G1 generation boundary.
// Every run owns its continuation, cancellation scope and finalizer.
export function createEventRunner({ instantiate, resetState, clearTimers, eventLimitMs = 1000,
  maxConcurrent = 32, retirementAdmissionLimit = 64 }) {
  if (!Number.isInteger(maxConcurrent) || maxConcurrent < 1 || maxConcurrent > 32) {
    throw new RangeError('maxConcurrent must be an integer from 1 to 32');
  }
  if (!Number.isInteger(retirementAdmissionLimit) || retirementAdmissionLimit < 1 || retirementAdmissionLimit > 96) {
    throw new RangeError('retirementAdmissionLimit must be an integer from 1 to 96');
  }
  let exports = instantiate();
  exports.__wasm_call_ctors();
  let generation = 1;
  let unavailable = false;
  const pending = new Set();

  // Bound retained allocation even if a dependency retains request App state.
  // Retire at the next idle boundary after the configured admission count; 96 is the hard ceiling.
  let admittedCount = 0;
  let queued = 0;
  function resetInstance() {
    unavailable = true;
    clearTimers();
    const previousMemory = exports.memory;
    resetState();
    exports = instantiate();
    if (exports.memory === previousMemory) throw new Error('Wasm memory was reused');
    exports.__wasm_call_ctors();
    admittedCount = 0;
    unavailable = false;
  }
  function rotate() {
    generation++;
    resetInstance();
  }
  // Failure is distinct from clean retirement: reject every admitted event, then
  // rebuild. Owner-context finalizers dispose I/O after rejection.
  function abandon(cause) {
    const abandoned = generation;
    const failure = new Error(`Wasm generation ${abandoned} abandoned: ${cause}`);
    failure.code = 'instance_abandoned';
    failure.generation = abandoned;
    unavailable = true;
    for (const event of pending) {
      clearTimeout(event.timer);
      event.invalidate();
      event.reject(failure);
    }
    pending.clear();
    try { rotate(); } catch { /* Admission remains closed on reset failure. */ }
  }

  function run(operation, { scope, signal } = {}) {
    if (unavailable) return Promise.reject(new Error('Wasm instance unavailable'));
    if (admittedCount >= 96) {
      if (!pending.size) {
        try { rotate(); } catch (error) { return Promise.reject(error); }
      } else {
        if (queued >= 32) return Promise.reject(new Error('Rotation queue capacity exceeded'));
        queued++;
        // Each waiter owns its timer. A shared cross-request Promise can be
        // canceled by workerd when its creating request has no local I/O left.
        return (async () => {
          try {
            const deadline = Date.now() + eventLimitMs;
            while (admittedCount >= 96 && pending.size) {
              if (unavailable) throw new Error('Wasm instance unavailable');
              if (Date.now() >= deadline) throw new Error('Rotation admission deadline exceeded');
              await new Promise(resolve => setTimeout(resolve, 1));
            }
            return await run(operation, { scope, signal });
          } finally { queued--; }
        })();
      }
    }
    if (pending.size >= maxConcurrent) return Promise.reject(new Error('Event capacity exceeded'));
    const admitted = generation;
    admittedCount++;
    let event;
    const result = new Promise((resolve, reject) => {
      const onAbort = () => scope?.abort();
      event = { reject, invalidate() { scope?.invalidate?.(); }, dispose() { scope?.abort(); signal?.removeEventListener('abort', onAbort); }, timer: setTimeout(() => abandon('event deadline exceeded'), eventLimitMs) };
      signal?.addEventListener('abort', onAbort, { once: true });
      if (signal?.aborted) onAbort();
      pending.add(event);
      let pendingOperation;
      try {
        pendingOperation = operation();
      } catch (error) {
        abandon(String(error));
        return;
      }
      Promise.resolve(pendingOperation).then(value => {
        if (!pending.has(event) || admitted !== generation) return;
        clearTimeout(event.timer);
        try {
          resolve({ ...JSON.parse(value), generation: admitted, wasm_memory_bytes: exports.memory.buffer.byteLength });
        } catch (error) { reject(error); }
      }, error => {
        if (admitted === generation) abandon(String(error));
      });
    });
    // Register cleanup in the owning fetch context. Another event may reject this
    // promise, but it must never directly abort this request's native I/O objects.
    return result.finally(async () => {
      try {
        event?.dispose();
        if (await scope?.settled?.() === false) throw new Error('Storage completion unavailable');
      } catch {
        const error = new Error('storage_cleanup_unconfirmed');
        error.status = 503;
        throw error;
      }
      finally {
        // Even a normal result can leave unconfirmed native work. Fence every
        // continuation before this event leaves the generation's pending set.
        event?.invalidate();
        pending.delete(event);
        if (!unavailable && !pending.size && admittedCount >= retirementAdmissionLimit) rotate();
      }
    });
  }
  return { run, generation: () => generation };
}
