import { run } from '../runner.mjs';
let boot;
export default {
  async fetch(request, env) {
    if (!env.PROBE_KEY || request.headers.get('x-probe-key') !== env.PROBE_KEY) return new Response('Not found', { status: 404 });
    boot ??= crypto.randomUUID();
    const path = new URL(request.url).pathname;
    if (request.method !== 'GET' || !['/probe', '/memory'].includes(path)) return new Response('Not found', { status: 404 });
    const retained = [];
    const scope = {
      abort() { retained.length = 0; },
      async exchange() {
        // Invoked by the Rust Host after Kernel Ready. Capped pressure, no infinite loop.
        for (let index = 0; index < 64; index++) {
          const bytes = new Uint8Array(8 * 1024 * 1024);
          bytes.fill(index + 1);
          retained.push(bytes);
          await new Promise(resolve => setTimeout(resolve, 1));
        }
        throw new Error('Platform did not terminate at bounded 512 MiB pressure');
      },
    };
    const result = await run('termination-proof', path === '/memory' ? 'io-memory' : 'normal', { scope });
    return Response.json({ ...result, boot });
  },
};
