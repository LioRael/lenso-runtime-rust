import http from 'node:http';
import https from 'node:https';
// Raw HTTP preserves repeated request field lines and permits TRACE, unlike Node Fetch.
export function send(base, vector, { pauseBodyMs = 0 } = {}) {
  const target = new URL(base);
  const transport = target.protocol === 'https:' ? https : http;
  return new Promise((resolve, reject) => {
    const headers = vector.headers.flat();
    if (!headers.some(value => String(value).toLowerCase() === 'host')) headers.push('Host', target.host);
    const body = Buffer.from(vector.body);
    if (pauseBodyMs) headers.push('Content-Length', String(body.length));
    let paused;
    const deadline = setTimeout(() => request.destroy(new Error('Smoke transport deadline')), 10000);
    const request = transport.request({ hostname: target.hostname, port: target.port || undefined, path: vector.uri, method: vector.method, headers, agent: false }, response => {
      const chunks = []; let size = 0;
      response.on('data', chunk => { size += chunk.length; if (size > 262144) response.destroy(new Error('Response exceeds smoke bound')); else chunks.push(chunk); });
      response.on('error', fail);
      response.on('end', () => {
        clearTimeout(deadline); clearTimeout(paused);
        resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks) });
        if (!request.writableEnded) request.end();
      });
    });
    function fail(error) { clearTimeout(deadline); clearTimeout(paused); reject(error); }
    request.on('error', fail);
    if (pauseBodyMs) {
      request.write(body.subarray(0, 1));
      paused = setTimeout(() => request.end(body.subarray(1)), pauseBodyMs);
    } else request.end(body);
  });
}

export async function fetchResponse(base, uri) {
  const response = await send(base, { method: 'GET', uri, headers: [], body: [] });
  const headers = Object.entries(response.headers).flatMap(([name, value]) =>
    (Array.isArray(value) ? value : [value]).map(item => [name, item]));
  return new Response([204, 205, 304].includes(response.status) ? null : response.body, { status: response.status, headers });
}
