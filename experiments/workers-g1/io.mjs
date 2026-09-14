// Explicit request-owned host facility. No ambient current-request registry.
export function createIoScope(origin, identity, { slow = false, delayed = false } = {}) {
  const controller = new AbortController();
  const pending = new Set();
  let closed = false;
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  const stats = { started: 0, headers: 0, completed: 0, aborted: 0 };
  return {
    ready,
    stats,
    get pendingCount() { return pending.size; },
    get aborted() { return controller.signal.aborted; },
    abort() { closed = true; controller.abort(); },
    async settled() { await Promise.allSettled([...pending]); },
    exchange(value) {
      if (value !== identity) return Promise.reject(new Error('Mismatched I/O scope'));
      if (closed) return Promise.resolve(JSON.stringify({ state: 'aborted' }));
      const operation = (async () => {
        stats.started++;
        const url = new URL(slow ? '/slow' : delayed ? '/delayed' : '/echo', origin);
        url.searchParams.set('value', value);
        try {
          const response = await fetch(url, {
            headers: { 'x-probe-identity': encodeURIComponent(identity) },
            signal: controller.signal,
            redirect: 'manual',
          });
          stats.headers++;
          started();
          if (!response.ok) throw new Error(`Upstream ${response.status}`);
          const reader = response.body.getReader();
          const chunks = [];
          let size = 0;
          try {
            for (;;) {
              const { value: chunk, done } = await reader.read();
              if (done) break;
              size += chunk.byteLength;
              if (size > 4096) throw new Error('Upstream body limit exceeded');
              chunks.push(chunk);
            }
          } finally { await reader.cancel(); }
          if (closed) throw new Error('I/O scope closed');
          const bytes = new Uint8Array(size);
          let offset = 0;
          for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
          const result = JSON.parse(new TextDecoder().decode(bytes));
          if (result.value !== identity || result.identity !== identity) throw new Error('Upstream identity mismatch');
          stats.completed++;
          return JSON.stringify({ state: 'ok', value: result.value });
        } catch (error) {
          if (!controller.signal.aborted) throw error;
          stats.aborted++;
          return JSON.stringify({ state: 'aborted' });
        }
      })();
      pending.add(operation);
      operation.then(() => { started(); pending.delete(operation); }, () => { started(); pending.delete(operation); });
      return operation;
    },
  };
}

// wasm-bindgen import takes the exact scope supplied to this Host invocation.
export function exchange(scope, value) { return scope.exchange(value); }
