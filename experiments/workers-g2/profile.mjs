// Disposable W02 proof composition. Exactly one call per Worker script/realm.
import * as generated from "./pkg/lenso_workers_g2_host.js";
import module from "./pkg/lenso_workers_g2_host_bg.wasm";
import {
  createEventRunner,
  createEventScope,
  createWorkersHttpHost,
  createStreamingHttpHandler,
} from "@lenso/workers-runtime";
import { clearTimers } from "@lenso/workers-runtime/clock";
import { createWebSocketTransport } from "./fixtures/websocket.mjs";
import identity from "./qualification-identity.json";

export const limits = Object.freeze({
  maxConcurrent: 32,
  retirementAdmissionLimit: 64,
  generationCeiling: 96,
  maxWaiters: 32,
  eventLimitMs: 1000,
  sessionLimitMs: 300000,
  cancellationLimitMs: 1000,
  cleanupTimeoutMs: 250,
  maxOperations: 128,
});
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
};
const require = (condition, message) => {
  if (!condition) throw Error(message);
};
const wire = (path) =>
  JSON.stringify({ method: "GET", uri: path, headers: [], body: [] });
export function createProfile(profile) {
  if (!["http", "session", "mixed"].includes(profile))
    throw Error("unknown profile");
  let diagnostic;
  let boot,
    generation = 1,
    sequence = 0,
    active = 0,
    highWater = 0,
    admissions = 0;
  let totalAdmissions = 0,
    cleanups = 0,
    failures = 0,
    dropped = 0,
    resets = 0;
  const receipts = new Map();
  const controls = new Map();
  const prune = () => {
    for (const [id, receipt] of receipts)
      if (Date.now() - receipt.at > 60000) {
        receipts.delete(id);
        dropped++;
      }
    while (receipts.size > 16) {
      receipts.delete(receipts.keys().next().value);
      dropped++;
    }
  };
  const snapshot = () => ({
    sourceSha256: identity.sourceSha256,
    wasmSha256: identity.wasmSha256,
    profile,
    script: `w02-${profile}`,
    boot,
    generation,
    limits,
    active,
    highWater,
    admissions,
    totalAdmissions,
    cleanups,
    failures,
    dropped,
    resets,
    diagnostic,
    counterBoundary:
      "instrumented business App entries through owner cleanup; diagnostic fault entries and transport reservations are excluded",
  });
  const receipt = (record) => {
    receipts.set(record.id, { ...record, at: Date.now() });
    prune();
  };
  function entered(record) {
    record.generation = generation;
    record.entered = true;
    record.phase = "entered";
    admissions++;
    totalAdmissions++;
    active++;
    highWater = Math.max(active, highWater);
    record.admission = admissions;
    require(admissions <= 96 &&
      active <= 32, "internal admission bound exceeded");
    receipt(record);
  }
  function finished(record, clean, error) {
    if (record.finished) return;
    record.finished = true;
    if (record.entered) {
      active--;
      cleanups++;
    }
    if (!clean) failures++;
    record.phase = "cleanup";
    record.clean = clean;
    record.error = error;
    receipt(record);
  }
  const bindings = {
    ...generated,
    __wbg_reset_state() {
      generated.__wbg_reset_state();
      generation++;
      admissions = 0;
      resets++;
    },
  };
  const newRecord = () => ({
    id: `${boot}:${++sequence}`,
    profile,
    script: `w02-${profile}`,
    boot,
  });
  let runner, http;
  if (profile === "http") {
    const records = new WeakMap();
    http = createWorkersHttpHost({
      bindings: {
        ...bindings,
        handle_http(input, scope) {
          entered(records.get(scope));
          return generated.handle_http(input, scope);
        },
      },
      wasmModule: module,
      limits: {
        maxRequestBodyBytes: 65536,
        maxResponseBodyBytes: 65536,
        maxRequestHeadBytes: 16384,
        bodyReadTimeoutMs: 250,
      },
      createScope(request) {
        const scope = createEventScope();
        records.set(scope, requestRecords.get(request));
        return scope;
      },
      onReceipt(result, response, request) {
        const record = requestRecords.get(request);
        record.wasmMemoryBytes = result.wasm_memory_bytes;
        finished(record, result.shutdown === "clean");
        response.headers.set("x-w02-generation", String(result.generation));
        response.headers.set("x-g2-shutdown", result.shutdown);
      },
    });
  } else
    runner = createEventRunner({
      instantiate: () => generated.initSync({ module }),
      resetState: bindings.__wbg_reset_state,
      clearTimers,
    });
  const requestRecords = new WeakMap();
  async function open(
    path,
    { scope = createEventScope(), record = newRecord(), signal } = {},
  ) {
    try {
      const session = await runner.open(
        async () => {
          entered(record);
          const response = await generated.open_http(wire(path), scope);
          return { value: response, closed: response.closed };
        },
        { scope, signal },
      );
      session.closed.then(
        () => finished(record, true),
        (error) => finished(record, false, String(error)),
      );
      return { session, scope, record };
    } catch (error) {
      finished(record, false, String(error));
      throw error;
    }
  }
  async function healthy() {
    const { session } = await open("/stream");
    const bytes = [];
    for (;;) {
      const part = await session.invoke(() => session.value.read());
      if (part === null) break;
      bytes.push(...part);
    }
    await session.closed;
    require(JSON.stringify(bytes) ===
      "[0,1,255,2,3,254]", "healthy Rust stream bytes");
    return { generation: session.generation, bytes };
  }
  async function rejected(call, pattern) {
    try {
      await call();
    } catch (error) {
      require(pattern.test(String(error)), `wrong failure: ${error}`);
      return String(error);
    }
    throw Error("expected rejection");
  }
  async function probe(name, barrier = false) {
    require(runner, "session/mixed proof only");
    const before = snapshot();
    if (name === "capacity") {
      const sessions = [];
      try {
        for (let index = 0; index < 32; index++)
          sessions.push((await open("/stream?hold")).session);
        const failure = await rejected(() => open("/stream"), /capacity/);
        require(active === 32, "capacity must retain all owners");
        return {
          passed: true,
          held: sessions.length,
          failure,
          atCapacity: snapshot(),
        };
      } finally {
        for (const session of sessions) {
          session.cancel();
          await session.closed;
        }
      }
    }
    if (name === "cancellation") {
      const peer = (await open("/stream?hold")).session;
      const cancelled = (await open("/stream?hold")).session;
      cancelled.cancel();
      cancelled.cancel();
      await cancelled.closed;
      require(peer.generation ===
        generation, "cooperative cancellation abandoned peer");
      require((await peer.invoke(() => peer.value.read())).length >
        0, "peer no longer readable");
      peer.cancel();
      await peer.closed;
      await rejected(
        () => open("/stream", { signal: AbortSignal.abort() }),
        /Abort/,
      );
      return {
        passed: true,
        before,
        after: snapshot(),
        recovery: await healthy(),
        source: "owner cancellation, not client disconnect",
      };
    }
    if (
      ["quarantine", "late-reject", "export-reject", "nonclean"].includes(name)
    ) {
      const native = deferred(),
        cleaning = deferred();
      const scope = createEventScope();
      let deliveries = 0;
      scope
        .operation(() => ({
          promise: native.promise,
          abort() {
            cleaning.resolve();
          },
        }))
        .promise.then(
          () => deliveries++,
          () => deliveries++,
        );
      const { session } = await open("/stream?hold", { scope });
      const memory = generated.initSync({ module }).memory;
      let failure;
      if (name === "nonclean") {
        const broken = await runner.open(async () => {
          // Actual Rust/Wasm lease, then inject only the host terminal receipt.
          const temporary = await generated.open_http(
            wire("/stream?hold"),
            createEventScope(),
          );
          return {
            value: temporary,
            closed: Promise.resolve({ shutdown: "unconfirmed" }),
          };
        });
        failure = await rejected(() => broken.closed, /abandoned/);
      } else
        failure = await rejected(
          () =>
            runner.run(() => {
              if (name === "export-reject")
                return generated.open_http("invalid", createEventScope());
              return generated.trap_probe();
            }),
          /abandoned/,
        );
      await cleaning.promise;
      const during = snapshot();
      const denied = await rejected(
        () =>
          runner.run(
            () => {
              throw Error("successor entered Wasm");
            },
            {
              prepare() {
                throw Error("successor buffered body");
              },
            },
          ),
        /unavailable/,
      );
      require(generated.initSync({ module }).memory !==
        memory, "reset reused memory");
      await rejected(
        () => session.invoke(() => session.value.read()),
        /session_closed/,
      );
      await rejected(
        () => session.value.read(),
        /instance|stale|reset|different|generation/i,
      );
      // Wait for the actual bounded cleanup receipt, not a timing guess.
      const terminal = await rejected(
        () => session.closed,
        /storage_cleanup_unconfirmed/,
      );
      await rejected(() => runner.run(() => "{}"), /unavailable/);
      if (barrier) {
        diagnostic = { state: "quarantined", proceed: false };
        const deadline = Date.now() + 5000;
        while (!diagnostic.proceed) {
          require(Date.now() <
            deadline, "external quarantine barrier deadline");
          await new Promise((resolve) => setTimeout(resolve, 1));
        }
        diagnostic = undefined;
      }
      native[name === "late-reject" ? "reject" : "resolve"](
        "late native completion",
      );
      // Each poll owns its timer; no cross-request shared reset Promise.
      let recovery;
      const deadline = Date.now() + 1000;
      while (!recovery) {
        try {
          recovery = await healthy();
        } catch (error) {
          if (!/unavailable/.test(String(error)) || Date.now() >= deadline)
            throw error;
          await new Promise((resolve) => setTimeout(resolve, 1));
        }
      }
      require(deliveries ===
        0, "stale native completion entered Wasm projection");
      return {
        passed: true,
        before,
        during,
        failure,
        denied,
        terminal,
        deliveries,
        memoryChanged: true,
        recovery,
        after: snapshot(),
        cleanupReceiptSticky: (await scope.settled()) === false,
      };
    }
    if (name === "cleanup-reject") {
      const nativeScope = createEventScope();
      const scope = {
        ...nativeScope,
        settled: () => Promise.reject(Error("unknown cleanup release")),
      };
      const { session } = await open("/stream?hold", { scope });
      const failure = await rejected(
        () => runner.run(() => generated.trap_probe()),
        /abandoned/,
      );
      const cleanup = await rejected(
        () => session.closed,
        /storage_cleanup_unconfirmed/,
      );
      const denied = await rejected(() => healthy(), /unavailable/);
      await rejected(
        () => session.invoke(() => session.value.read()),
        /session_closed/,
      );
      return {
        passed: true,
        failure,
        cleanup,
        denied,
        recovery: "new isolate required: custom cleanup release unknown",
        after: snapshot(),
      };
    }
    if (name === "baseline") {
      require(profile === "mixed", "baseline requires mixed domain");
      const { session } = await open("/stream?hold");
      try {
        for (let index = 0; index < 95; index++) {
          const scope = createEventScope();
          const record = newRecord();
          await runner.run(
            () => {
              entered(record);
              return generated.handle_http(wire("/method"), scope);
            },
            { scope },
          );
          finished(record, true);
        }
        const failure = await rejected(
          () => runner.run(() => "{}"),
          /admission deadline/,
        );
        require(admissions === 96, "baseline count");
        return { passed: true, failure, held: snapshot() };
      } finally {
        session.cancel();
        await session.closed;
      }
    }
    throw Error("unknown probe");
  }
  return {
    async fetch(request, env, ctx) {
      boot ??= crypto.randomUUID();
      prune();
      const path = new URL(request.url).pathname;
      if (path === "/_w02/meta")
        return Response.json({ ...identity, ...snapshot() });
      if (path === "/_w02/continue") {
        if (diagnostic) diagnostic.proceed = true;
        return Response.json({ found: !!diagnostic });
      }
      if (path === "/_w02/release") {
        const id = new URL(request.url).searchParams.get("id");
        const control = controls.get(id);
        if (control) control.release = true;
        return Response.json({ found: !!control });
      }
      if (path === "/_w02/receipt") {
        const id = new URL(request.url).searchParams.get("id");
        return Response.json({
          ...snapshot(),
          found: receipts.has(id),
          receipt: receipts.get(id),
        });
      }
      if (path.startsWith("/_w02/probe/")) {
        try {
          const result = await probe(
            path.slice(12),
            new URL(request.url).searchParams.has("barrier"),
          );
          return Response.json({ ...result, after: snapshot() });
        } catch (error) {
          return Response.json(
            { passed: false, error: String(error), after: snapshot() },
            { status: 500 },
          );
        }
      }
      const sessionRoute = path === "/stream" || /^\/socket\/[^/]+$/.test(path);
      if (profile !== "mixed" && sessionRoute !== (profile === "session")) {
        await request.body?.cancel().catch(() => {});
        return Response.json(
          { error: "host_unavailable" },
          {
            status: 503,
            headers: {
              "x-w02-profile": profile,
              "x-w02-script": `w02-${profile}`,
              "x-w02-boot": boot,
              "x-w02-rejection": "route_profile_mismatch",
            },
          },
        );
      }
      const record = newRecord();
      requestRecords.set(request, record);
      let response;
      if (profile === "http") response = await http.fetch(request, env, ctx);
      else {
        const handler = createStreamingHttpHandler({
          open: async (operation, options) => {
            try {
              const session = await runner.open((...args) => {
                entered(record);
                return operation(...args);
              }, options);
              // The proof release signal is JS-only; cancellation runs on the owner's timer.
              // It proves deterministic close independently of downstream disconnect propagation.
              const control = { release: false };
              controls.set(record.id, control);
              const timer = setInterval(() => {
                if (control.release) session.cancel();
              }, 1);
              const closed = session.closed
                .then(
                  () => finished(record, true),
                  (error) => finished(record, false, String(error)),
                )
                .finally(() => {
                  clearInterval(timer);
                  controls.delete(record.id);
                });
              ctx?.waitUntil(closed);
              return session;
            } catch (error) {
              finished(record, false, String(error));
              throw error;
            }
          },
          async openHttp(input, scope) {
            const response = await generated.open_http(input, scope);
            return {
              value: {
                status: response.status,
                headers: JSON.parse(response.headers),
                read: () => response.read(),
                send: (frame) => response.send(JSON.stringify(frame)),
              },
              closed: response.closed,
            };
          },
          upgradeWebSocket: createWebSocketTransport(),
        });
        response = await handler(request);
      }
      for (const [key, value] of Object.entries({
        profile,
        script: `w02-${profile}`,
        boot,
        generation: record.generation ?? generation,
        event: record.id,
        artifact: identity.wasmSha256,
      }))
        response.headers.set(`x-w02-${key}`, String(value));
      return response;
    },
  };
}
