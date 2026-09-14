import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
const base = process.env.WORKERS_G2_URL;
assert.ok(base);
const id = randomUUID();
const cancellation = new AbortController();
const response = await fetch(new URL('/_g2/disconnect?id=' + id, base), { signal: cancellation.signal });
assert.equal(response.status, 200);
const reader = response.body.getReader();
let text = '';
while (!text.includes('\n')) {
  const chunk = await reader.read();
  assert.equal(chunk.done, false);
  text += new TextDecoder().decode(chunk.value);
}
const identity = JSON.parse(text.slice(0, text.indexOf('\n')));
cancellation.abort();
await reader.cancel().catch(() => {});
console.error(JSON.stringify({ disconnect_identity: identity }));
let receipt;
for (let attempt = 0; attempt < 30; attempt++) {
  await new Promise(resolve => setTimeout(resolve, 100));
  const value = await fetch(new URL('/_g2/receipt?id=' + id, base), { signal: AbortSignal.timeout(5000) }).then(response => response.json());
  if (value.found && value.boot === identity.boot) { receipt = value.receipt; break; }
}
const passed = receipt?.cancelled === 'true' && receipt?.request_aborted === true && receipt?.signal === 'request' && receipt?.status === 503 && receipt?.shutdown === 'clean';
console.log(JSON.stringify({ passed, base, receipt }));
assert.ok(receipt, 'No same-isolate completion receipt');
assert.equal(receipt.cancelled, 'true', JSON.stringify(receipt));
assert.equal(receipt.request_aborted, true, JSON.stringify(receipt));
assert.equal(receipt.signal, 'request', JSON.stringify(receipt));
assert.equal(receipt.status, 503, JSON.stringify(receipt));
assert.equal(receipt.shutdown, 'clean');
