import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdir, writeFile } from "node:fs/promises";
import WebSocket from "ws";
const origin = process.env.DUPLEX_URL;
const deadline = setTimeout(() => {
  console.error("qualification timed out");
  process.exit(1);
}, 45000);
if (!origin) throw new Error("DUPLEX_URL is required");
const results = [];
async function check(name, run) {
  await run();
  results.push({ name, passed: true });
  console.log(name);
}
const wsOrigin = origin.replace(/^http/, "ws");
function socket(path = "/socket/room", options = {}) {
  return new WebSocket(wsOrigin + path, "lenso.echo", {
    headers: {
      authorization: "Bearer proof",
      origin: "https://client.invalid",
    },
    ...options,
  });
}
await check("stream preserves binary bytes and reaches EOF", async () => {
  const response = await fetch(origin + "/stream");
  assert.equal(response.status, 200);
  assert.deepEqual(
    [...new Uint8Array(await response.arrayBuffer())],
    [0, 1, 255, 2, 3, 254],
  );
});
await check(
  "held stream delivers bytes before completion and allows consumer cancellation",
  async () => {
    const response = await fetch(origin + "/stream?hold");
    assert.equal(response.status, 200);
    const reader = response.body.getReader();
    const first = await reader.read();
    assert.ok(first.value.length > 0);
    assert.equal(first.done, false);
    await reader.cancel();
  },
);
await check(
  "WebSocket text binary empty frames subprotocol and close",
  async () => {
    const ws = socket();
    await once(ws, "open");
    assert.equal(ws.protocol, "lenso.echo");
    for (const [payload, binary] of [
      ["你好", false],
      [Buffer.from([0, 255, 1]), true],
      ["", false],
      [Buffer.alloc(0), true],
    ]) {
      const next = once(ws, "message");
      ws.send(payload, { binary });
      const [received, isBinary] = await next;
      assert.equal(isBinary, binary);
      assert.deepEqual(Buffer.from(received), Buffer.from(payload));
    }
    const closed = once(ws, "close");
    ws.close(1000, "complete");
    const [code, reason] = await closed;
    assert.equal(code, 1000);
    assert.equal(reason.toString(), "complete");
  },
);
for (const [name, path, headers, status] of [
  [
    "authorization before upgrade",
    "/socket/room",
    { origin: "https://client.invalid" },
    401,
  ],
  [
    "provider denial before upgrade",
    "/socket/denied",
    { authorization: "Bearer proof", origin: "https://client.invalid" },
    403,
  ],
  [
    "origin rejected before upgrade",
    "/socket/room",
    { authorization: "Bearer proof", origin: "https://untrusted.invalid" },
    400,
  ],
])
  await check(name, async () => {
    await new Promise((resolve, reject) => {
      const ws = socket(path, { headers });
      ws.on("error", reject);
      ws.on("unexpected-response", (_request, response) => {
        try {
          assert.equal(response.statusCode, status);
          response.resume();
          _request.destroy();
          resolve();
        } catch (error) {
          reject(error);
        }
      });
      ws.on("open", () => {
        ws.terminate();
        reject(new Error("unexpected upgrade"));
      });
    });
  });
await check(
  "healthy stream after cancelled and rejected sessions",
  async () => {
    assert.equal((await fetch(origin + "/stream")).status, 200);
  },
);
await check(
  "oversized inbound frame closes without poisoning another session",
  async () => {
    const healthy = socket();
    await once(healthy, "open");
    const oversized = socket();
    await once(oversized, "open");
    const closed = once(oversized, "close");
    oversized.send("x".repeat(65537));
    const [code] = await closed;
    assert.equal(code, 1009);
    const message = once(healthy, "message");
    healthy.send("still alive");
    assert.equal((await message)[0].toString(), "still alive");
    const finished = once(healthy, "close");
    healthy.close();
    await finished;
  },
);
await check(
  "abrupt peer disconnect does not poison subsequent upgrade",
  async () => {
    const dropped = socket();
    await once(dropped, "open");
    dropped.terminate();
    const healthy = socket();
    await once(healthy, "open");
    const received = once(healthy, "message");
    healthy.send("after disconnect");
    assert.equal((await received)[0].toString(), "after disconnect");
    const finished = once(healthy, "close");
    healthy.close();
    await finished;
  },
);
clearTimeout(deadline);
const evidence = {
  version: process.env.WORKER_VERSION,
  url: origin,
  at: new Date().toISOString(),
  count: results.length,
  results,
};
if (process.env.EVIDENCE)
  await mkdir(
    new URL(
      ".",
      new URL(process.env.EVIDENCE, "file://" + process.cwd() + "/"),
    ),
    { recursive: true },
  );
if (process.env.EVIDENCE)
  await writeFile(
    process.env.EVIDENCE,
    JSON.stringify(evidence, null, 2) + "\n",
  );
console.log(JSON.stringify({ passed: true, count: results.length }));
