// Bounded diagnostic receipt store; not product state or an installation API.
import { run } from './runner.mjs';
import { createIoScope } from './io.mjs';
const receipts = new Map();
export async function disconnectProof(request, env, ctx, boot) {
  const url = new URL(request.url);
  for (const [id, receipt] of receipts) if (Date.now() - receipt.created > 60000) receipts.delete(id);
  if (url.pathname === '/disconnect/status') {
    const id = url.searchParams.get('id');
    const result = receipts.get(id);
    return Response.json(result ?? { missing: true }, { status: result ? 200 : 404, headers: { 'cache-control': 'no-store', 'x-probe-boot': boot } });
  }
  if (url.pathname !== '/disconnect' || request.method !== 'GET') return new Response('Not found', { status: 404 });
  if (receipts.size >= 16) return new Response('Receipt capacity exceeded', { status: 429 });
  const id = crypto.randomUUID();
  const state = { id, boot, created: Date.now(), cancelled: false, done: false };
  receipts.set(id, state);
  const scope = createIoScope(env.UPSTREAM_BASE, id, { slow: true });
  const cancellation = new AbortController();
  const onDisconnect = () => { state.cancelled = true; state.request_aborted = request.signal.aborted; state.signal = state.request_aborted ? 'request' : 'response-write'; cancellation.abort(); };
  request.signal.addEventListener('abort', onDisconnect, { once: true });
  if (request.signal.aborted) onDisconnect();
  const operation = run(id, 'io-slow', { scope, signal: cancellation.signal });
  const { readable, writable } = new IdentityTransformStream();
  const writer = writable.getWriter();
  const close = () => writer.close().catch(() => {});
  ctx.waitUntil(operation.then(async result => {
    await scope.settled();
    Object.assign(state, { done: true, result, io: { ...scope.stats, pending: scope.pendingCount } });
  }, error => { Object.assign(state, { done: true, error: String(error), code: error.code }); }).finally(() => { request.signal.removeEventListener('abort', onDisconnect); console.log("G1_DISCONNECT", JSON.stringify(state)); return close(); }));
  await scope.ready;
  ctx.waitUntil(writer.write(new TextEncoder().encode(JSON.stringify({ id, boot }) + '\n' + ' '.repeat(32768))).catch(() => onDisconnect()));
  return new Response(readable, { headers: { 'content-type': 'text/plain', 'cache-control': 'no-store, no-transform', 'content-encoding': 'identity', 'x-probe-boot': boot } });
}
