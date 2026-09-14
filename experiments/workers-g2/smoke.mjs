import assert from 'node:assert/strict';
import { send as sendHttp, fetchResponse } from './transport.mjs';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
const base = process.env.WORKERS_G2_URL ?? 'http://127.0.0.1:63737';
const corpusPath = new URL('../../../../lenso-web/feat-workers-http-parity/tests/fixtures/http-parity-plugin/corpus.json', import.meta.url);
const corpusBytes = await readFile(corpusPath);
const corpus = JSON.parse(corpusBytes);
const send = vector => sendHttp(base, vector);
const network = [];
for (const vector of corpus) {
  const result = await send(vector);
  const platformRejected = !result.headers['x-g2-shutdown'];
  if (platformRejected) {
    network.push({ name: vector.name, passed: false, boundary: 'transport-without-host-receipt', status: result.status, expected: vector.status, content_type: result.headers['content-type'], server: result.headers.server });
    continue;
  }
  assert.equal(result.status, vector.status, vector.name);
  assert.equal(result.headers['x-g2-shutdown'], 'clean', vector.name);
  assert.equal(result.headers['x-content-type-options'], 'nosniff', vector.name);
  assert.notEqual(result.headers['x-request-id'], 'untrusted', vector.name);
  if (vector.response_body) assert.deepEqual([...result.body], vector.response_body, vector.name);
  if (vector.set_cookie_count) assert.equal(result.headers['set-cookie'].length, vector.set_cookie_count, vector.name);
  if (vector.route || 'query' in vector || vector.credential) {
    const body = JSON.parse(result.body);
    for (const [source, dest] of [['route', 'route_id'], ['path', 'path'], ['query', 'query'], ['credential', 'credential']]) {
      if (source in vector) assert.deepEqual(body[dest], vector[source], `${vector.name}: ${source}`);
    }
  }
  const observedHeaders = vector.name === 'repeated-headers' ? JSON.parse(result.body).headers.filter(header => header.name === 'x-test') : undefined;
  network.push({ name: vector.name, passed: true, status: result.status, observed_request_headers: observedHeaders });
}
const request = url => { const parsed = new URL(url); return fetchResponse(base, parsed.pathname + parsed.search); };
const bridged = await request(base + '/_g2/corpus').then(async response => {
  const value = await response.json(); assert.equal(response.status, 200, JSON.stringify(value)); return value;
});
assert.equal(bridged.passed, true);
assert.equal(bridged.count, corpus.length);
const cancellation = await request(base + '/_g2/cancellation').then(response => response.json());
assert.equal(cancellation.passed, true, JSON.stringify(cancellation));
const recovery = await request(base + '/_g2/recovery').then(response => response.json());
assert.equal(recovery.passed, true, JSON.stringify(recovery));
const deadline = await request(base + '/blocked');
assert.equal(deadline.status, 504);
assert.equal(deadline.headers.get('x-g2-shutdown'), 'clean');
const oversized = await send({ method: 'POST', uri: '/bytes', headers: [], body: new Uint8Array(65537) });
assert.equal(oversized.status, 413);
let healthyAfterLimits = false;
let healthFailure;
try {
  const healthy = await request(base + '/method');
  assert.equal(await healthy.text(), 'GET');
  healthyAfterLimits = true;
} catch (error) { healthFailure = String(error); }
console.log(JSON.stringify({ base, at: new Date().toISOString(), corpus_sha256: createHash('sha256').update(corpusBytes).digest('hex'), corpus_count: corpus.length, network, bridged, cancellation, recovery, deadline: 504, request_body_limit: 413, healthy_after_limits: healthyAfterLimits, health_failure: healthFailure }, null, 2));
assert.equal(healthyAfterLimits, true, healthFailure);
