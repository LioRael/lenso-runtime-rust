// Shared assertions for the external Node client and supplementary workerd test.
const assert = (value, message) => {
  if (!value) throw Error(message);
};
export async function qualify({
  http,
  session,
  mixed,
  fetch: request = fetch,
  socket,
  selected = "all",
}) {
  const cases = [],
    terminalReceipts = [],
    shorts = [];
  let requests = 0;
  const get = async (url, options) => {
    requests++;
    return request(url, options);
  };
  const json = async (url) => {
    const response = await get(url);
    const result = await response.json();
    assert(response.ok, JSON.stringify(result));
    return result;
  };
  const h = await json(http + "/_w02/meta"),
    s = await json(session + "/_w02/meta");
  assert(
    h.profile === "http" && h.script === "w02-http",
    "exact HTTP identity",
  );
  assert(
    s.profile === "session" && s.script === "w02-session",
    "exact session identity",
  );
  assert(
    h.boot !== s.boot &&
      h.wasmSha256 === s.wasmSha256 &&
      /^[a-f0-9]{64}$/.test(h.wasmSha256),
    "separate boots, exact artifact",
  );
  for (const meta of [h, s])
    assert(
      JSON.stringify(meta.limits) ===
        JSON.stringify({
          maxConcurrent: 32,
          retirementAdmissionLimit: 64,
          generationCeiling: 96,
          maxWaiters: 32,
          eventLimitMs: 1000,
          sessionLimitMs: 300000,
          cancellationLimitMs: 1000,
          cleanupTimeoutMs: 250,
          maxOperations: 128,
        }),
      "fixed limits changed",
    );
  async function check(name, fn) {
    if (selected !== "all" && selected !== name) return;
    try {
      const evidence = await fn();
      cases.push({ name, passed: true, evidence });
    } catch (error) {
      cases.push({ name, passed: false, error: String(error) });
    }
  }
  function identity(response, expected) {
    assert(
      response.headers.get("x-w02-profile") === expected.profile,
      "response profile",
    );
    assert(
      response.headers.get("x-w02-script") === expected.script,
      "response script",
    );
    assert(
      response.headers.get("x-w02-boot") === expected.boot,
      "missing same-boot evidence",
    );
    assert(
      response.headers.get("x-w02-artifact") === expected.wasmSha256,
      "response artifact",
    );
  }
  async function receipt(origin, id, terminal = false) {
    const deadline = Date.now() + 2000;
    for (;;) {
      const result = await json(
        origin + "/_w02/receipt?id=" + encodeURIComponent(id),
      );
      assert(result.found, `missing receipt ${id}`);
      if (!terminal || result.receipt.phase === "cleanup") {
        terminalReceipts.push(result.receipt);
        return result.receipt;
      }
      assert(Date.now() < deadline, "terminal/cleanup receipt deadline");
      await new Promise((resolve) => setTimeout(resolve, 1));
    }
  }
  async function short() {
    const response = await get(http + "/method");
    identity(response, h);
    assert(
      response.status === 200 && (await response.text()) === "GET",
      "short request failed",
    );
    assert(response.headers.get("x-g2-shutdown") === "clean", "short shutdown");
    const result = await receipt(
      http,
      response.headers.get("x-w02-event"),
      true,
    );
    assert(result.clean, "short owner cleanup");
    shorts.push(result);
    return result;
  }
  await check("routing", async () => {
    const results = [];
    for (const [origin, path, expected] of [
      [http, "/stream?hold", h],
      [http, "/socket/room", h],
      [session, "/method", s],
    ]) {
      const before = await json(origin + "/_w02/meta");
      const response = await get(origin + path, {
        headers: {
          "x-w02-profile": expected.profile === "http" ? "session" : "http",
        },
      });
      assert(
        response.status === 503 &&
          response.headers.get("x-w02-rejection") === "route_profile_mismatch",
        "route mismatch did not fail closed",
      );
      assert(
        response.headers.get("x-w02-profile") === expected.profile,
        "spoofed profile",
      );
      await response.arrayBuffer();
      assert(
        (await json(origin + "/_w02/meta")).totalAdmissions ===
          before.totalAdmissions,
        "mismatch entered Wasm",
      );
      results.push({ origin, path, status: response.status });
    }
    return results;
  });
  await check("baseline", async () => {
    assert(mixed, "explicit mixed origin required");
    const result = await json(mixed + "/_w02/probe/baseline");
    assert(result.passed, JSON.stringify(result));
    assert(
      result.held.admissions === 96 &&
        result.held.active === 1 &&
        result.after.active === 0,
      "baseline capacity/cleanup",
    );
    return result;
  });
  async function held(kind) {
    const before = await json(session + "/_w02/meta");
    let id, close, read;
    if (kind === "stream") {
      const response = await get(session + "/stream?hold");
      identity(response, s);
      assert(response.status === 200, "held stream status");
      id = response.headers.get("x-w02-event");
      const reader = response.body.getReader();
      assert((await reader.read()).value.length > 0, "stream open barrier");
      close = () => reader.cancel();
      read = async () => {};
    } else {
      assert(socket, "WebSocket adapter missing");
      const ws = await socket(session + "/socket/room");
      identity({ headers: ws.headers }, s);
      id = ws.headers.get("x-w02-event");
      close = ws.close;
      read = () => ws.exchange("still held");
      await read();
    }
    const live = await receipt(session, id);
    const start = shorts.length;
    try {
      for (let index = 0; index < 192; index++) {
        await short();
        if (index % 64 === 63) await read();
      }
      const after = await json(session + "/_w02/meta");
      assert(
        after.generation === live.generation &&
          after.active === before.active + 1,
        "held owner retired or closed",
      );
      const generations = [
        ...new Set(shorts.slice(start).map((r) => r.generation)),
      ];
      assert(generations.length >= 3, "no same-boot HTTP retirement");
      return {
        count: 192,
        httpGenerations: generations,
        sessionGeneration: live.generation,
        sessionBoot: s.boot,
      };
    } finally {
      if (kind === "stream")
        assert(
          (await json(session + "/_w02/release?id=" + encodeURIComponent(id)))
            .found,
          "owner release control missing",
        );
      await close();
      const terminal = await receipt(session, id, true);
      assert(terminal.clean, "held session cleanup unconfirmed");
    }
  }
  await check("held-stream", () => held("stream"));
  await check("held-websocket", () => held("websocket"));
  for (const name of [
    "capacity",
    "cancellation",
    "quarantine",
    "late-reject",
    "export-reject",
    "nonclean",
    "cleanup-reject",
  ])
    await check(name, async () => {
      let result, healthyDuringQuarantine;
      if (
        ["quarantine", "late-reject", "export-reject", "nonclean"].includes(
          name,
        )
      ) {
        const pending = json(session + "/_w02/probe/" + name + "?barrier");
        pending.catch(() => {});
        const deadline = Date.now() + 3000;
        for (;;) {
          const meta = await json(session + "/_w02/meta");
          if (meta.diagnostic?.state === "quarantined") break;
          assert(Date.now() < deadline, "quarantine barrier missing");
          await new Promise((resolve) => setTimeout(resolve, 1));
        }
        healthyDuringQuarantine = await short();
        assert(
          (await json(session + "/_w02/continue")).found,
          "quarantine release barrier missing",
        );
        result = await pending;
      } else result = await json(session + "/_w02/probe/" + name);
      result.healthyDuringQuarantine = healthyDuringQuarantine;
      assert(result.passed, JSON.stringify(result));
      await short();
      assert(result.after.active === 0, "probe owner cleanup missing");
      return result;
    });
  assert(cases.length > 0, "unknown case selector");
  return {
    schema: "w02-local-matrix-v1",
    passed: cases.every((entry) => entry.passed),
    identities: { http: h, session: s },
    requests,
    successfulShortRequests: shorts.length,
    shortGenerations: [...new Set(shorts.map((r) => r.generation))],
    cases,
    terminalReceipts,
    missingReceipts: cases.filter((c) => /receipt/.test(c.error || "")).length,
    claim:
      "local-only; no production, external IdP, fleet/product capacity or resource isolation claim",
    exclusions: [
      "Cloudflare deployment/route configuration",
      "external IdP",
      "platform CPU/memory termination",
      "storage-provider uncertainty",
      "G1 sustained-retirement workload",
      "real client disconnect unless separately evidenced",
    ],
  };
}
