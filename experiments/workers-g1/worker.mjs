import { run, recovery } from './runner.mjs';
import { createIoScope } from './io.mjs';
import { ioCancellation, ioRecovery } from './io-proof.mjs';
let boot;
export default {
  async fetch(request, env) {
    boot ??= crypto.randomUUID();
    const url = new URL(request.url);
    if (request.method !== 'GET' || url.pathname !== '/probe') return new Response('Not found', { status: 404 });
    const input = url.searchParams.get('input') ?? 'hello';
    const mode = url.searchParams.get('mode') ?? 'normal';
    if (input.length > 1024 || !['normal', 'missing-factory', 'startup-failure', 'trap', 'driver', 'async-trap', 'task-trap', 'async-panic', 'async-pending', 'recovery', 'conformance', 'lifecycle', 'io-exchange', 'io-delayed', 'io-slow', 'io-cancel', 'io-recovery'].includes(mode)) return new Response('Invalid probe', { status: 400 });
    const scope = ['io-exchange', 'io-delayed', 'io-slow'].includes(mode) ? createIoScope(env.UPSTREAM_BASE, input, { delayed: mode === 'io-delayed', slow: mode === 'io-slow' }) : undefined;
    try {
      const result = mode === 'io-cancel' ? await ioCancellation(env.UPSTREAM_BASE)
        : mode === 'io-recovery' ? await ioRecovery(env.UPSTREAM_BASE)
        : mode === 'recovery' ? await recovery()
        : await run(input, mode, { signal: request.signal, scope });
      return Response.json(result, { headers: { 'cache-control': 'no-store', 'x-probe-boot': boot } });
    } catch (error) {
      await scope?.settled();
      return Response.json({ failed: true, code: error.code ?? 'host_failure', detail: String(error), shutdown: 'unconfirmed', io: scope ? { ...scope.stats, pending: scope.pendingCount } : undefined }, { status: 500, headers: { 'cache-control': 'no-store', 'x-probe-boot': boot } });
    }
  }
};
