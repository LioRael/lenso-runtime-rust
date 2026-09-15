import { writeFile, mkdir } from "node:fs/promises";
import { dirname } from "node:path";
import { gzipSync } from "node:zlib";
import WebSocket from "ws";
import { qualify } from "./qualification-matrix.mjs";
const options = {};
for (let i = 2; i < process.argv.length; i += 2) {
  if (
    !["--http", "--session", "--mixed", "--case", "--evidence"].includes(
      process.argv[i],
    ) ||
    !process.argv[i + 1]
  )
    throw Error(
      "Usage: node qualify-local.mjs --http ORIGIN --session ORIGIN --mixed ORIGIN --case all --evidence PATH",
    );
  options[process.argv[i].slice(2)] = process.argv[i + 1];
}
if (!options.evidence || !options.http || !options.session)
  throw Error("HTTP/session origins and evidence path required");
for (const origin of [options.http, options.session, options.mixed].filter(
  Boolean,
)) {
  const url = new URL(origin);
  if (!["127.0.0.1", "localhost", "[::1]"].includes(url.hostname))
    throw Error("This harness is local-only");
}
const sockets = new Set();
const request = (url, options = {}) =>
  fetch(url, { ...options, signal: AbortSignal.timeout(5000) });
async function socket(url) {
  const ws = new WebSocket(url.replace(/^http/, "ws"), "lenso.echo", {
    headers: {
      authorization: "Bearer proof",
      origin: "https://client.invalid",
    },
    handshakeTimeout: 5000,
  });
  sockets.add(ws);
  let headers;
  ws.on("upgrade", (response) => {
    headers = new Headers(response.headers);
  });
  // Observe errors throughout the socket's lifetime, including cleanup.
  ws.on("error", () => {});
  const wait = (name) =>
    new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        cleanup();
        reject(Error(`${name} deadline`));
      }, 5000);
      const done = (...value) => {
        cleanup();
        resolve(value);
      };
      const fail = (error) => {
        cleanup();
        reject(error);
      };
      const cleanup = () => {
        clearTimeout(timer);
        ws.off(name, done);
        ws.off("error", fail);
      };
      ws.once(name, done);
      ws.once("error", fail);
    });
  await wait("open");
  return {
    headers,
    async exchange(value) {
      const next = wait("message");
      ws.send(value);
      if ((await next)[0].toString() !== value)
        throw Error("WebSocket echo differs");
    },
    async close() {
      const closed = wait("close");
      ws.close(1000, "complete");
      await closed;
      sockets.delete(ws);
    },
  };
}
let evidence;
const startedAt = new Date().toISOString();
const deadline = setTimeout(() => {
  for (const ws of sockets) ws.terminate();
}, 60000);
try {
  evidence = await qualify({
    ...options,
    selected: options.case || "all",
    fetch: request,
    socket,
  });
} catch (error) {
  evidence = {
    schema: "w02-local-matrix-v1",
    passed: false,
    status: "missing-required-evidence",
    error: String(error),
    cases: [],
  };
} finally {
  clearTimeout(deadline);
  for (const ws of sockets) ws.terminate();
}
evidence = {
  ...evidence,
  startedAt,
  finishedAt: new Date().toISOString(),
  argv: process.argv.slice(2),
  execution: "external local HTTP/WebSocket client",
  claim:
    "local-only; no Cloudflare production, external IdP or product capacity claim",
};
await mkdir(dirname(options.evidence), { recursive: true });
await writeFile(
  options.evidence,
  gzipSync(JSON.stringify(evidence, null, 2) + "\n", { level: 9 }),
);
console.log(
  JSON.stringify({
    passed: evidence.passed,
    cases: evidence.cases?.length,
    evidence: options.evidence,
  }),
);
if (!evidence.passed) process.exitCode = 1;
