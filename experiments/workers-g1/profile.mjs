// Run against the local experimental Worker, started with inspector port 9229.
import fs from 'node:fs';
import WebSocket from 'ws';

const [target] = await (await fetch('http://127.0.0.1:9229/json/list')).json();
const socket = new WebSocket(target.webSocketDebuggerUrl, {
  headers: { Origin: 'https://devtools.devprod.cloudflare.dev' },
});
await new Promise((resolve, reject) => {
  socket.addEventListener('open', resolve, { once: true });
  socket.addEventListener('error', reject, { once: true });
});
let nextId = 0;
const pending = new Map();
socket.addEventListener('message', event => {
  const message = JSON.parse(event.data);
  const request = pending.get(message.id);
  if (!request) return;
  pending.delete(message.id);
  clearTimeout(request.timer);
  if (message.error) request.reject(message.error);
  else request.resolve(message.result);
});
function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`Inspector timeout: ${method}`));
    }, 5000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

try {
  const heaps = [];
  await send('Profiler.enable');
  await send('Profiler.setSamplingInterval', { interval: 1000 });
  await send('Profiler.start');
  for (let batch = 0; batch < 20; batch++) {
    const results = await Promise.all(Array.from({ length: 12 }, (_, i) =>
      fetch(`http://127.0.0.1:63733/probe?input=load-${batch}-${i}`, {
        signal: AbortSignal.timeout(5000),
      }).then(response => response.json()),
    ));
    if (results.some(result => result.shutdown !== 'clean' || result.invocations !== '1')) {
      throw new Error('Load verification failed');
    }
    heaps.push(await send('Runtime.getHeapUsage'));
  }
  const { profile } = await send('Profiler.stop');
  fs.writeFileSync(process.env.WORKERS_G1_PROFILE_PATH ?? '/tmp/workers-g1-profile.cpuprofile', JSON.stringify(profile));
  const nodes = new Map(profile.nodes.map(node => [node.id, node]));
  let nonidle = 0;
  let idle = 0;
  profile.samples.forEach((sample, index) => {
    if (nodes.get(sample).callFrame.functionName === '(idle)') idle += profile.timeDeltas[index];
    else nonidle += profile.timeDeltas[index];
  });
  console.log(JSON.stringify({
    requests: 240,
    concurrency: 12,
    profile_duration_us: profile.endTime - profile.startTime,
    sampled_nonidle_us: nonidle,
    sampled_idle_us: idle,
    heap_samples: heaps.length,
    max_observed_heap_used: Math.max(...heaps.map(heap => heap.usedSize)),
    first: heaps[0],
    last: heaps.at(-1),
    note: 'Local workerd V8 CPU samples and batch-boundary heap observations; not remote billed CPU or absolute peak process memory.',
  }, null, 2));
} finally {
  for (const request of pending.values()) clearTimeout(request.timer);
  socket.close();
}
