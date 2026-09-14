import assert from 'node:assert/strict';
import fs from 'node:fs';
const base = process.env.WORKERS_G1_TERMINATION_URL;
assert.ok(base, 'Set WORKERS_G1_TERMINATION_URL');
const key = fs.readFileSync(process.env.WORKERS_G1_KEY_FILE, 'utf8').trim();
const options = () => ({ headers: { 'x-probe-key': key }, signal: AbortSignal.timeout(15000) });
async function healthy() {
  const response = await fetch(new URL('/probe', base), options());
  assert.equal(response.status, 200);
  const value = await response.json();
  assert.equal(value.shutdown, 'clean');
  assert.equal(value.body, 'termination-proof');
  return value;
}
const denied = await fetch(new URL('/cpu', base));
assert.equal(denied.status, 404);
const before = await healthy();
const failure = await fetch(new URL('/cpu', base), options());
const body = await failure.text();
assert.ok(failure.status >= 500, 'Expected platform failure HTTP status');
assert.ok(body.includes('1102'), 'Expected Cloudflare resource limit error 1102');
const after = await healthy();
console.log(JSON.stringify({ passed: true, http_status: failure.status, platform_error: 1102, cpu_limit_ms: 10, before_boot: before.boot, after_boot: after.boot, following_request: 'healthy', forced_shutdown: 'unconfirmed', note: 'Boot IDs are observations across HTTP requests, not proof that the same isolate was selected or evicted.' }));
