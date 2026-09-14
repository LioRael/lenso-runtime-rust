import assert from 'node:assert/strict';
const base = process.env.WORKERS_G1_URL;
assert.ok(base);
const cancellation = new AbortController();
const response = await fetch(new URL('/disconnect', base), { signal: cancellation.signal });
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
  const url = new URL('/disconnect/status', base);
  url.searchParams.set('id', identity.id);
  const status = await fetch(url, { signal: AbortSignal.timeout(5000) });
  const value = await status.json();
  if (value.done && value.boot === identity.boot) { receipt = value; break; }
}
assert.ok(receipt, 'No same-isolate completion receipt');
assert.equal(receipt.cancelled, true, JSON.stringify(receipt));
assert.equal(receipt.request_aborted, true, JSON.stringify(receipt));
assert.equal(receipt.result?.io, 'aborted', JSON.stringify(receipt));
assert.equal(receipt.result.shutdown, 'clean');
assert.equal(receipt.io.aborted, 1);
assert.equal(receipt.io.pending, 0);
console.log(JSON.stringify({ passed: true, base, receipt }));
