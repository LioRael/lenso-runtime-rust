import { initSync, __wbg_reset_state, probe, trap_probe, driver_probe, conformance_probe, lifecycle_probe, named_dependencies_probe, diagnostics_probe, interactions_probe, schedules_probe, bindings_probe } from './pkg/lenso_workers_g1_host.js';
import { clearTimers } from './clock.mjs';
import module from './pkg/lenso_workers_g1_host_bg.wasm';

import { createEventRunner } from '../workers-runtime/runner.mjs';
const runner = createEventRunner({
  instantiate: () => initSync({ module }),
  resetState: __wbg_reset_state,
  clearTimers,
});
export function run(input, mode, options = {}) {
  return runner.run(() => {
    if (mode === 'trap') trap_probe();
    return mode === 'interactions' ? interactions_probe() : mode === 'schedules' ? schedules_probe() : mode === 'bindings' ? bindings_probe() : mode === 'named-dependencies' ? named_dependencies_probe() : mode === 'diagnostics' ? diagnostics_probe() : mode === 'lifecycle' ? lifecycle_probe() : mode === 'conformance' ? conformance_probe() : mode === 'driver' ? driver_probe() : probe(input, mode, options.scope);
  }, options);
}

// All operations run inside one fetch event, so recovery evidence cannot be
// explained by Cloudflare routing the next HTTP request to another isolate.
export async function recovery() {
  const before = await run('before', 'normal');
  const faultGeneration = runner.generation();
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
