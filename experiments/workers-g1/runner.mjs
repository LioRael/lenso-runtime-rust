import { initSync, __wbg_reset_state, probe, trap_probe, driver_probe, conformance_probe, lifecycle_probe, named_dependencies_probe, diagnostics_probe, interactions_probe, schedules_probe, bindings_probe } from './pkg/lenso_workers_g1_host.js';
import { clearTimers } from './clock.mjs';
import module from './pkg/lenso_workers_g1_host_bg.wasm';

let exports = initSync({ module });
exports.__wasm_call_ctors();
let generation = 1;
let unavailable = false;
const pending = new Set();
const eventLimitMs = 1000;

// Bound retained allocation even if a dependency retains request App state.
// Retire at the next idle boundary after 64 admissions; 96 is the hard ceiling.
let admittedCount = 0;
let queued = 0;
function resetInstance() {
  unavailable = true;
  clearTimers();
  const previousMemory = exports.memory;
  __wbg_reset_state();
  exports = initSync({ module });
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
    event.reject(failure);
  }
  pending.clear();
  try { rotate(); } catch { /* Admission remains closed on reset failure. */ }
}

export function run(input, mode, { scope, signal } = {}) {
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
          return await run(input, mode, { scope, signal });
        } finally { queued--; }
      })();
    }
  }
  if (pending.size >= 32) return Promise.reject(new Error('Event capacity exceeded'));
  const admitted = generation;
  admittedCount++;
  let event;
  const result = new Promise((resolve, reject) => {
    const onAbort = () => scope?.abort();
    event = { reject, dispose() { scope?.abort(); signal?.removeEventListener('abort', onAbort); }, timer: setTimeout(() => abandon('event deadline exceeded'), eventLimitMs) };
    signal?.addEventListener('abort', onAbort, { once: true });
    if (signal?.aborted) onAbort();
    pending.add(event);
    let operation;
    try {
      if (mode === 'trap') trap_probe();
      operation = mode === 'interactions' ? interactions_probe() : mode === 'schedules' ? schedules_probe() : mode === 'bindings' ? bindings_probe() : mode === 'named-dependencies' ? named_dependencies_probe() : mode === 'diagnostics' ? diagnostics_probe() : mode === 'lifecycle' ? lifecycle_probe() : mode === 'conformance' ? conformance_probe() : mode === 'driver' ? driver_probe() : probe(input, mode, scope);
    } catch (error) {
      abandon(String(error));
      return;
    }
    Promise.resolve(operation).then(value => {
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
    try { event?.dispose(); await scope?.settled?.(); }
    finally {
      pending.delete(event);
      if (!unavailable && !pending.size && admittedCount >= 64) rotate();
    }
  });
}

// All operations run inside one fetch event, so recovery evidence cannot be
// explained by Cloudflare routing the next HTTP request to another isolate.
export async function recovery() {
  const before = await run('before', 'normal');
  const faultGeneration = generation;
  const failed = await Promise.allSettled([
    run('trap', 'async-trap'),
    run('pending', 'async-pending'),
  ]);
  const after = await run('after', 'normal');
  if (failed.some(result => result.status !== 'rejected' || result.reason.code !== 'instance_abandoned' || result.reason.generation !== faultGeneration)) {
    throw new Error('A trapped generation was not rejected');
  }
  if (after.generation <= faultGeneration || after.invocations !== '1') throw new Error('Instance was not recreated');
  return { recovery: 'passed', rejected_events: failed.length, before: before.generation, after: after.generation, shutdown: 'unconfirmed-for-abandoned-generation' };
}
