export default {
  async fetch(request) {
    const url = new URL(request.url);
    if (request.method !== 'GET' || !['/echo', '/slow', '/delayed'].includes(url.pathname)) return new Response('Not found', { status: 404 });
    const value = url.searchParams.get('value') ?? '';
    if (value.length > 1024) return new Response('Too large', { status: 413 });
    const encoded = new TextEncoder().encode(JSON.stringify({ value, identity: decodeURIComponent(request.headers.get('x-probe-identity') ?? '') }));
    if (url.pathname === '/echo') return new Response(encoded, { headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });
    const abort = new AbortController();
    const body = new ReadableStream({
      async start(controller) {
        controller.enqueue(encoded.slice(0, 1));
        try { await scheduler.wait(url.pathname === '/slow' ? 3000 : 200, { signal: abort.signal }); }
        catch { return; }
        controller.enqueue(encoded.slice(1));
        controller.close();
      },
      cancel() { abort.abort(); },
    });
    return new Response(body, { headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });
  },
};
