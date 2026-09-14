import { handleRequest, recovery, responseLimitProof } from './runner.mjs';
import corpus from '../../../../lenso-web/feat-workers-http-parity/tests/fixtures/http-parity-plugin/corpus.json';

let boot;
const receipts = new Map();
function prune() {
  for (const [id, receipt] of receipts) if (Date.now() - receipt.at > 60000) receipts.delete(id);
  while (receipts.size > 16) receipts.delete(receipts.keys().next().value);
}
function input(vector, origin) {
  return new Request(origin + vector.uri, {
    method: vector.method, headers: vector.headers,
    body: ['GET', 'HEAD'].includes(vector.method) ? undefined : Uint8Array.from(vector.body),
  });
}
async function examine(vector, response) {
  const bytes = [...new Uint8Array(await response.arrayBuffer())];
  const failures = [];
  if (response.status !== vector.status) failures.push(`status ${response.status} != ${vector.status}`);
  if (response.headers.get('x-content-type-options') !== 'nosniff') failures.push('missing nosniff');
  if (response.headers.get('x-request-id') === 'untrusted') failures.push('trusted external request id');
  if (response.headers.get('x-g2-shutdown') !== 'clean') failures.push('shutdown not confirmed');
  if (vector.response_body && JSON.stringify(bytes) !== JSON.stringify(vector.response_body)) failures.push('body differs');
  if (vector.set_cookie_count && response.headers.getSetCookie().length !== vector.set_cookie_count) failures.push('cookie multiplicity differs');
  if (vector.route || 'query' in vector || vector.credential) {
    const body = JSON.parse(new TextDecoder().decode(Uint8Array.from(bytes)));
    for (const [source, destination] of [['route', 'route_id'], ['path', 'path'], ['query', 'query'], ['credential', 'credential']]) {
      if (source in vector && JSON.stringify(body[destination]) !== JSON.stringify(vector[source])) failures.push(`${source} differs`);
    }
  }
  return { name: vector.name, status: response.status, failures };
}

export default {
  async fetch(request, env, ctx) {
    boot ??= crypto.randomUUID();
    const url = new URL(request.url);
    if (url.pathname === '/_g2/body-timeout') {
      let timer;
      const body = new ReadableStream({
        start(controller) {
          controller.enqueue(new Uint8Array([97]));
          timer = setTimeout(() => { controller.enqueue(new Uint8Array([98])); controller.close(); }, 1000);
        },
        cancel() { clearTimeout(timer); },
      });
      return handleRequest(new Request(url.origin + '/bytes', { method: 'POST', body }));
    }
    if (url.pathname === '/_g2/response-limit') return responseLimitProof(new Request(url.origin + '/bytes', { method: 'POST', body: new Uint8Array(8) }));
    if (url.pathname === '/_g2/recovery') return Response.json(await recovery(url.origin));
    if (url.pathname === '/_g2/corpus') {
      const results = [];
      for (const vector of corpus) {
        try { results.push(await examine(vector, await handleRequest(input(vector, url.origin)))); }
        catch (error) { results.push({ name: vector.name, failures: [String(error)] }); }
      }
      const passed = results.every(result => result.failures.length === 0);
      return Response.json({ passed, count: results.length, results, boot, boundary: 'Workers Request/Response through shared Web Ingress and Kernel' }, { status: passed ? 200 : 500 });
    }
    if (url.pathname === '/_g2/cancellation') {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), 20);
      try {
        const [cancelled, healthy] = await Promise.all([
          handleRequest(new Request(url.origin + '/blocked', { signal: controller.signal })),
          handleRequest(new Request(url.origin + '/method')),
        ]);
        const passed = cancelled.status === 503 && cancelled.headers.get('x-g2-cancelled') === 'true'
          && cancelled.headers.get('x-g2-shutdown') === 'clean' && healthy.status === 200
          && healthy.headers.get('x-g2-shutdown') === 'clean' && await healthy.text() === 'GET';
        return Response.json({ passed, cancelled: cancelled.status, cancellation_observed: cancelled.headers.get('x-g2-cancelled'), healthy: healthy.status, boot }, { status: passed ? 200 : 500 });
      } finally { clearTimeout(timer); }
    }
    if (url.pathname === '/_g2/disconnect') {
      const id = url.searchParams.get('id');
      if (!id || !/^[a-zA-Z0-9-]{1,64}$/.test(id)) return new Response('Invalid receipt id', { status: 400 });
      const cancellation = new AbortController();
      let signal = 'none';
      const onDisconnect = () => { signal = request.signal.aborted ? 'request' : 'response-write'; cancellation.abort(); };
      request.signal.addEventListener('abort', onDisconnect, { once: true });
      if (request.signal.aborted) onDisconnect();
      const { readable, writable } = new IdentityTransformStream();
      const writer = writable.getWriter();
      const work = handleRequest(new Request(url.origin + '/blocked', { signal: cancellation.signal })).then(response => {
        const receipt = { id, boot, status: response.status, cancelled: response.headers.get('x-g2-cancelled'), shutdown: response.headers.get('x-g2-shutdown'), request_aborted: request.signal.aborted, signal, at: Date.now() };
        receipts.set(id, receipt); prune();
        console.log(JSON.stringify({ kind: 'g2-client-disconnect', ...receipt }));
      }).finally(async () => {
        request.signal.removeEventListener('abort', onDisconnect);
        await writer.close().catch(() => {});
      });
      ctx.waitUntil(work);
      // Force an observable streaming boundary rather than a buffered small response.
      ctx.waitUntil(writer.write(new TextEncoder().encode(JSON.stringify({ id, boot }) + '\n' + ' '.repeat(32768))).catch(onDisconnect));
      return new Response(readable, { headers: { 'content-type': 'text/plain', 'cache-control': 'no-store, no-transform', 'content-encoding': 'identity', 'x-probe-boot': boot } });
    }
    if (url.pathname === '/_g2/receipt') {
      prune();
      const receipt = receipts.get(url.searchParams.get('id'));
      return Response.json({ found: Boolean(receipt), receipt, boot });
    }
    return handleRequest(request);
  },
};
