import assert from 'node:assert/strict';
const base = process.env.WORKERS_G1_URL;
assert.ok(base, 'Set WORKERS_G1_URL');
async function read(input, mode = 'normal') {
  const url = new URL('/probe', base);
  url.search = new URLSearchParams({ input, mode }).toString();
  const response = await fetch(url, { signal: AbortSignal.timeout(15000) });
  const body = await response.text();
  assert.equal(response.status, 200, body);
  return JSON.parse(body);
}
function healthy(value, input) {
  assert.equal(value.factories, 2);
  assert.equal(value.ready, true);
  assert.equal(value.body, input);
  assert.equal(value.invocations, '1');
  assert.equal(value.shutdown, 'clean');
  assert.equal(value.timer, 'passed');
  assert.equal(value.cancellation, 'passed');
  assert.equal(value.invocation_cancellation, 'passed');
  assert.equal(value.deadline, 'passed');
  assert.ok(value.wasm_memory_bytes > 0);
}
healthy(await read('first'), 'first');
const inputs = Array.from({ length: 12 }, (_, i) => `isolated-${i}-你好`);
const concurrent = await Promise.all(inputs.map(input => read(input)));
concurrent.forEach((value, i) => healthy(value, inputs[i]));
for (const mode of ['missing-factory', 'startup-failure']) {
  const result = await read('reject', mode);
  assert.equal(result.rejected, true);
  assert.equal(result.mode, mode);
}
const driver = await read('driver', 'driver');
assert.equal(driver.task_bound, 128);
assert.equal(driver.parking, 'passed');
assert.equal(driver.shutdown_cancellation, 'passed');
const trap = await fetch(new URL('/probe?mode=trap', base), { signal: AbortSignal.timeout(15000) });
assert.equal(trap.status, 500);
assert.equal((await trap.json()).failed, true);
healthy(await read('after-failure'), 'after-failure');
const lifecycle = await read('lifecycle', 'lifecycle');
assert.equal(lifecycle.lifecycle, 'passed');
assert.deepEqual(lifecycle.cases, ['normal', 'prepare-failure', 'activate-failure', 'deactivate-failure', 'release-failure', 'shutdown-timeout', 'supervision']);
const contract = await read('conformance', 'conformance');
assert.equal(contract.conformance, 'passed');
assert.equal(contract.providers, 2);
const recovered = await read('recovery', 'recovery');
assert.equal(recovered.recovery, 'passed');
assert.equal(recovered.rejected_events, 2);
assert.ok(recovered.after > recovered.before);
assert.equal(recovered.shutdown, 'unconfirmed-for-abandoned-generation');
healthy(await read('after-recovery'), 'after-recovery');
for (const mode of ['task-trap', 'async-panic']) {
  const response = await fetch(new URL(`/probe?mode=${mode}`, base), { signal: AbortSignal.timeout(5000) });
  assert.equal(response.status, 500);
  const failure = await response.json();
  assert.equal(failure.code, 'instance_abandoned');
  assert.equal(failure.shutdown, 'unconfirmed');
  healthy(await read(`after-${mode}`), `after-${mode}`);
}
console.log(JSON.stringify({ base, passed: true, concurrent_requests: inputs.length, checks: ['static registration', 'Ready', 'bound Capability invocation', 'event state isolation', 'timer', 'task cancellation', 'missing factory', 'prepare failure', 'clean shutdown', 'recreation', 'bounded tasks', 'shutdown wakes parked lane', 'cancelled invocation', 'expired invocation', 'synchronous Wasm trap rejection', 'upstream request conformance vectors', 'lifecycle rollback and exactly-once cleanup', 'shutdown admission and timeout', 'stable handle after restart and restart exhaustion', 'async trap deadline', 'spawned task trap', 'async Plugin panic', 'same-event instance recreation', 'in-flight generation rejection'] }));
