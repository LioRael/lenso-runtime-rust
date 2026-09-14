import { run } from '../runner.mjs';
let boot;
export default {
  async fetch(request, env) {
    if (!env.PROBE_KEY || request.headers.get('x-probe-key') !== env.PROBE_KEY) return new Response('Not found', { status: 404 });
    boot ??= crypto.randomUUID();
    const path = new URL(request.url).pathname;
    if (request.method !== 'GET' || !['/probe', '/cpu'].includes(path)) return new Response('Not found', { status: 404 });
    const result = await run('termination-proof', path === '/cpu' ? 'cpu-limit' : 'normal');
    return Response.json({ ...result, boot });
  },
};
