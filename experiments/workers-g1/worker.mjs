import { initSync, probe, trap_probe, driver_probe } from './pkg/lenso_workers_g1_host.js';
import module from './pkg/lenso_workers_g1_host_bg.wasm';
// Initialization only instantiates code and static registrations, with no I/O.
const exports = initSync({ module });
exports.__wasm_call_ctors();
export default {
  async fetch(request) {
    const url = new URL(request.url);
    if (request.method !== 'GET' || url.pathname !== '/probe') return new Response('Not found', { status: 404 });
    const input = url.searchParams.get('input') ?? 'hello';
    const mode = url.searchParams.get('mode') ?? 'normal';
    if (input.length > 1024 || !['normal', 'missing-factory', 'startup-failure', 'trap', 'driver'].includes(mode)) return new Response('Invalid probe', { status: 400 });
    try {
      if (mode === 'trap') trap_probe();
      const result = JSON.parse(mode === 'driver' ? await driver_probe() : await probe(input, mode));
      result.wasm_memory_bytes = exports.memory.buffer.byteLength;
      return new Response(JSON.stringify(result), { headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });
    } catch (error) {
      return Response.json({ failed: true, detail: String(error) }, { status: 500 });
    }
  }
};
