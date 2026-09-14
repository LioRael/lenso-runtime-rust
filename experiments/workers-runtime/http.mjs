// Event-owned transport adaptation; routing and authorization stay in Web Ingress.
export function createCancellationScope(extra = {}) {
  let callback;
  let cancelled = false;
  return {
    ...extra,
    attach(next) { callback = next; if (cancelled) callback(); },
    detach() { callback = undefined; },
    // Called before Wasm reset. This must never invoke native I/O or Rust code.
    invalidate() { callback = undefined; },
    abort() { cancelled = true; callback?.(); },
    async settled() {},
  };
}
export function attachCancellation(scope, callback) { scope.attach(callback); }
export function detachCancellation(scope) { scope.detach(); }
export function cancellation(scope, callback) {
  if (callback === null) scope.detach(); else scope.attach(callback);
}

function transportFailure(status, error) {
  return Response.json({ error }, { status, headers: {
    'cache-control': 'no-store', 'x-content-type-options': 'nosniff',
  } });
}

function responseBytes(result, limit) {
  const fail = message => { const error = new Error(message); error.status = 502; throw error; };
  if (Object.hasOwn(result, 'body_base64')) {
    if (Object.hasOwn(result, 'body') || typeof result.body_base64 !== 'string') fail('invalid_response_body');
    const encoded = result.body_base64;
    // Bound allocation before decoding. Require canonical padded standard Base64.
    if (encoded.length > 4 * Math.ceil(limit / 3)) fail('response_body_too_large');
    const padding = encoded.endsWith('==') ? 2 : encoded.endsWith('=') ? 1 : 0;
    if (encoded.length % 4 !== 0 || /[^A-Za-z0-9+/]/.test(encoded.slice(0, encoded.length - padding))) {
      fail('invalid_response_body');
    }
    if (encoded.length / 4 * 3 - padding > limit) fail('response_body_too_large');
    const decoded = atob(encoded);
    if (btoa(decoded) !== encoded) fail('invalid_response_body');
    const bytes = new Uint8Array(decoded.length);
    for (let index = 0; index < decoded.length; index++) bytes[index] = decoded.charCodeAt(index);
    return bytes;
  }
  if (!Array.isArray(result.body)) fail('invalid_response_body');
  if (result.body.length > limit) fail('response_body_too_large');
  if (result.body.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) fail('invalid_response_body');
  return Uint8Array.from(result.body);
}

async function readBody(request, limit, timeoutMs) {
  const length = request.headers.get('content-length');
  if (length !== null && /^\d+$/.test(length) && Number(length) > limit) {
    const error = new Error('payload_too_large'); error.status = 413; throw error;
  }
  if (request.signal.aborted) throw new DOMException('Request aborted', 'AbortError');
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  let timer;
  let abort;
  const interrupted = new Promise((_, reject) => {
    abort = () => reject(new DOMException('Request aborted', 'AbortError'));
    request.signal.addEventListener('abort', abort, { once: true });
    timer = setTimeout(() => {
      const error = new Error('request_body_timeout'); error.status = 408; reject(error);
    }, timeoutMs);
  });
  const chunks = [];
  let lengthRead = 0;
  let complete = false;
  try {
    while (true) {
      const { value, done } = await Promise.race([reader.read(), interrupted]);
      if (done) { complete = true; break; }
      if (value.byteLength > limit - lengthRead) {
        const error = new Error('payload_too_large'); error.status = 413; throw error;
      }
      lengthRead += value.byteLength;
      chunks.push(value);
    }
    const bytes = new Uint8Array(lengthRead);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return bytes;
  } finally {
    clearTimeout(timer);
    request.signal.removeEventListener('abort', abort);
    if (!complete) await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export function createHttpHandler({ run, handleHttp, maxRequestBodyBytes = 1048576,
  maxResponseBodyBytes = 1048576, maxRequestHeadBytes = 16384, bodyReadTimeoutMs = 30000,
  createScope = () => createCancellationScope(), onReceipt = () => {} }) {
  return async function handleRequest(request) {
    let scope;
    try {
      scope = createScope(request);
      // URL.search loses a trailing empty '?'. Slice the original platform URL.
      const url = new URL(request.url);
      const uri = request.url.slice(url.origin.length).split('#', 1)[0] || '/';
      const headers = [...request.headers];
      const encoder = new TextEncoder();
      let headBytes = encoder.encode(request.method + ' ' + uri).byteLength;
      for (const [name, value] of headers) headBytes += encoder.encode(name + ': ' + value).byteLength + 2;
      if (headBytes > maxRequestHeadBytes) return transportFailure(431, 'request_header_fields_too_large');
      const body = await readBody(request, maxRequestBodyBytes, bodyReadTimeoutMs);
      const input = JSON.stringify({ method: request.method, uri, headers, body: [...body] });
      const result = await run(() => handleHttp(input, scope), { scope, signal: request.signal });
      if (result.shutdown !== 'clean') throw new Error('HTTP App shutdown was not clean');
      const responseBody = responseBytes(result, maxResponseBodyBytes);
      const responseHeaders = new Headers();
      for (const [name, value] of result.headers) responseHeaders.append(name, value);
      const bodyForbidden = request.method === 'HEAD' || [101, 204, 205, 304].includes(result.status);
      const response = new Response(bodyForbidden ? null : responseBody, {
        status: result.status, headers: responseHeaders,
      });
      onReceipt(result, response);
      return response;
    } catch (error) {
      if (error.name === 'AbortError') return transportFailure(503, 'request_cancelled');
      return transportFailure(error.status ?? 503, error.status ? error.message : 'host_unavailable');
    } finally {
      // Early head/length rejection must release an unread incoming body too.
      if (request.body && !request.bodyUsed && !request.body.locked) {
        await request.body.cancel().catch(() => {});
      }
      // Runner also finalizes admitted events; this covers body-read/admission errors.
      try {
        scope?.abort();
        const settled = await scope?.settled?.();
        // A bounded storage adapter can report uncertainty without retrying writes.
        if (settled === false) return transportFailure(503, 'storage_cleanup_unconfirmed');
      } catch {
        return transportFailure(503, 'storage_cleanup_unconfirmed');
      }
    }
  };
}
