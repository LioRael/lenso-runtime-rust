// Bounded, successful-request workload. Run fault probes separately.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
const base = process.env.WORKERS_G1_URL;
assert.ok(base, 'Set WORKERS_G1_URL');
const mode = process.env.WORKERS_G1_LOAD_MODE ?? 'normal';
assert.ok(['normal', 'io-exchange', 'io-delayed'].includes(mode));
const concurrency = Number(process.env.WORKERS_G1_CONCURRENCY ?? 12);
const count = Number(process.env.WORKERS_G1_REQUESTS ?? 240);
assert.ok(Number.isInteger(concurrency) && concurrency >= 1 && concurrency <= 24);
assert.ok(Number.isInteger(count) && count >= concurrency && count <= 2400);
const run = randomUUID();
const samples = [];
let next = 0;
const started = new Date().toISOString();
const begin = performance.now();
await Promise.all(Array.from({ length: concurrency }, async () => {
  while (next < count) {
    const index = next++;
    const input = `${run}-${index}-你好`.padEnd(Number(process.env.WORKERS_G1_INPUT_SIZE ?? 128), 'x');
    assert.ok(input.length <= 1024);
    const url = new URL('/probe', base);
    url.search = new URLSearchParams({ mode, input });
    const start = performance.now();
    const response = await fetch(url, { signal: AbortSignal.timeout(30000) });
    const result = await response.json();
    assert.equal(response.status, 200, JSON.stringify(result));
    assert.equal(result.body, input);
    assert.equal(result.shutdown, 'clean');
    assert.equal(result.invocations, '1');
    assert.ok(Number.isSafeInteger(result.wasm_memory_bytes) && result.wasm_memory_bytes > 0);
    const boot = response.headers.get('x-probe-boot');
    assert.ok(boot);
    samples.push({ index, latency_ms: performance.now() - start, boot,
      generation: result.generation, wasm_memory_bytes: result.wasm_memory_bytes });
  }
}));
const elapsed = performance.now() - begin;
const latencies = samples.map(s => s.latency_ms).sort((a,b) => a-b);
const percentile = p => latencies[Math.ceil(count*p)-1];
console.log(JSON.stringify({ run, base, mode, started, ended: new Date().toISOString(),
  passed: true, requests: count, concurrency, elapsed_ms: elapsed,
  throughput_requests_per_second: count*1000/elapsed,
  latency_ms: { p50: percentile(.5), p95: percentile(.95), p99: percentile(.99), max: latencies.at(-1) },
  observed_boots: new Set(samples.map(s => s.boot)).size,
  max_observed_wasm_capacity_bytes: Math.max(...samples.map(s => s.wasm_memory_bytes)),
  note: 'Closed-loop synthetic workload including startup and shutdown. Latency includes client network. Wasm capacity is neither live allocation nor total isolate peak memory. No production SLA claim.',
  samples: samples.sort((a,b) => a.index-b.index)
}, null, 2));
