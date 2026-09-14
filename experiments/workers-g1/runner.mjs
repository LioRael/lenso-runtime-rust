import { initSync, __wbg_reset_state, probe, trap_probe, driver_probe, conformance_probe, lifecycle_probe } from './pkg/lenso_workers_g1_host.js';
import { clearTimers } from './clock.mjs';
import module from './pkg/lenso_workers_g1_host_bg.wasm';

let exports = initSync({ module });
exports.__wasm_call_ctors();
let generation = 1;
let unavailable = false;
const pending = new Set();
const eventLimitMs = 1000;

// A Wasm trap can strand a Rust Future without rejecting its JS Promise. The
// embedding Runner owns this independent deadline; it is not a Kernel outcome.
function abandon(cause) {
  const abandoned = generation++;
  const failure = new Error(`Wasm generation ${abandoned} abandoned: ${cause}`);
  failure.code = 'instance_abandoned';
  failure.generation = abandoned;
  unavailable = true;
  for (const event of pending) {
    clearTimeout(event.timer);
    event.dispose();
    event.reject(failure);
  }
  pending.clear();
  try {
    clearTimers();
    const previousMemory = exports.memory;
    __wbg_reset_state();
    exports = initSync({ module });
    if (exports.memory === previousMemory) throw new Error("Wasm memory was reused");
    exports.__wasm_call_ctors();
    unavailable = false;
  } catch {
    // A failed reinitialization must not admit another request to broken state.
  }
}

export function run(input, mode, { scope, signal } = {}) {
  if (unavailable) return Promise.reject(new Error('Wasm instance unavailable'));
  if (pending.size >= 32) return Promise.reject(new Error('Event capacity exceeded'));
  const admitted = generation;
  return new Promise((resolve, reject) => {
    const onAbort = () => scope?.abort();
    const event = { reject, dispose() { scope?.abort(); signal?.removeEventListener('abort', onAbort); }, timer: setTimeout(() => abandon('event deadline exceeded'), eventLimitMs) };
    signal?.addEventListener('abort', onAbort, { once: true });
    if (signal?.aborted) onAbort();
    pending.add(event);
    let operation;
    try {
      if (mode === 'trap') trap_probe();
      operation = mode === 'lifecycle' ? lifecycle_probe() : mode === 'conformance' ? conformance_probe() : mode === 'driver' ? driver_probe() : probe(input, mode, scope);
    } catch (error) {
      abandon(String(error));
      return;
    }
    Promise.resolve(operation).then(value => {
      if (!pending.delete(event) || admitted !== generation) return;
      clearTimeout(event.timer);
      event.dispose();
      try {
        resolve({ ...JSON.parse(value), generation: admitted, wasm_memory_bytes: exports.memory.buffer.byteLength });
      } catch (error) { reject(error); }
    }, error => {
      if (admitted === generation) abandon(String(error));
    });
  });
}

// All operations run inside one fetch event, so recovery evidence cannot be
// explained by Cloudflare routing the next HTTP request to another isolate.
export async function recovery() {
  const before = await run('before', 'normal');
  const failed = await Promise.allSettled([
    run('trap', 'async-trap'),
    run('pending', 'async-pending'),
  ]);
  const after = await run('after', 'normal');
  if (failed.some(result => result.status !== 'rejected' || result.reason.code !== 'instance_abandoned' || result.reason.generation !== before.generation)) {
    throw new Error('A trapped generation was not rejected');
  }
  if (after.generation <= before.generation || after.invocations !== '1') throw new Error('Instance was not recreated');
  return { recovery: 'passed', rejected_events: failed.length, before: before.generation, after: after.generation, shutdown: 'unconfirmed-for-abandoned-generation' };
}
