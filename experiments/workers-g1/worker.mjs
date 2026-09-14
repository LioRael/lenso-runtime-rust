import { run, recovery } from './runner.mjs';
export default {
  async fetch(request) {
    const url = new URL(request.url);
    if (request.method !== 'GET' || url.pathname !== '/probe') return new Response('Not found', { status: 404 });
    const input = url.searchParams.get('input') ?? 'hello';
    const mode = url.searchParams.get('mode') ?? 'normal';
    if (input.length > 1024 || !['normal', 'missing-factory', 'startup-failure', 'trap', 'driver', 'async-trap', 'task-trap', 'async-panic', 'async-pending', 'recovery', 'conformance'].includes(mode)) return new Response('Invalid probe', { status: 400 });
    try {
      const result = mode === 'recovery' ? await recovery() : await run(input, mode);
      return Response.json(result, { headers: { 'cache-control': 'no-store' } });
    } catch (error) {
      return Response.json({ failed: true, code: error.code ?? 'host_failure', detail: String(error), shutdown: 'unconfirmed' }, { status: 500, headers: { 'cache-control': 'no-store' } });
    }
  }
};
