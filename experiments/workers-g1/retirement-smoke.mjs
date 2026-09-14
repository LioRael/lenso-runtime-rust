import assert from 'node:assert/strict';
import fs from 'node:fs';
const load = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
assert.equal(load.passed, true);
assert.ok(load.requests >= 1000, 'Use a sustained load receipt');
const boots = new Map();
for (const sample of load.samples) {
  if (!boots.has(sample.boot)) boots.set(sample.boot, new Map());
  const generations = boots.get(sample.boot);
  if (!generations.has(sample.generation)) generations.set(sample.generation, []);
  generations.get(sample.generation).push(sample.wasm_memory_bytes);
}
let resets = 0;
for (const generations of boots.values()) {
  const ordered = [...generations].sort((a,b) => a[0]-b[0]);
  for (const [, values] of ordered) assert.ok(values.length <= 96, 'Generation admission budget exceeded');
  for (let i = 1; i < ordered.length; i++) {
    if (Math.min(...ordered[i][1]) < Math.max(...ordered[i-1][1])) resets++;
  }
}
assert.ok(resets >= 2, 'Expected repeated same-isolate memory capacity reductions');
assert.ok(load.max_observed_wasm_capacity_bytes <= 8 * 1024 * 1024, 'Reference workload exceeded 8 MiB Wasm bound');
console.log(JSON.stringify({ passed: true, run: load.run, requests: load.requests,
  same_isolate_capacity_reductions: resets, max_wasm_bytes: load.max_observed_wasm_capacity_bytes,
  generation_soft_limit: 64, generation_hard_limit: 96, queue_capacity: 32 }));
