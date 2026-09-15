import {
  initSync,
  __wbg_reset_state,
  handle_http,
  open_http,
  parity_corpus,
  trap_probe,
} from "./pkg/lenso_workers_g2_host.js";
import module from "./pkg/lenso_workers_g2_host_bg.wasm";
import { clearTimers } from "./clock.mjs";
import { createEventRunner } from "../../packages/workers-runtime/runner.mjs";
import { createWebSocketTransport } from "./fixtures/websocket.mjs";
import {
  createHttpHandler,
  createStreamingHttpHandler,
} from "../../packages/workers-runtime/http.mjs";

const runner = createEventRunner({
  instantiate: () => initSync({ module }),
  resetState: __wbg_reset_state,
  clearTimers,
});
// Build-time proof option, never selected by an incoming request. Run the same
// G2 suites once with the compatibility default and once with explicit opt-in.
const requestBodyEncoding = typeof G2_REQUEST_BODY_ENCODING === "undefined"
  ? "numeric-array"
  : G2_REQUEST_BODY_ENCODING;
const bridgeOptions = {
  requestBodyEncoding,
  run: runner.run,
  handleHttp: handle_http,
  maxRequestBodyBytes: 65536,
  maxResponseBodyBytes: 65536,
  maxRequestHeadBytes: 16384,
  bodyReadTimeoutMs: 250,
  onReceipt(result, response) {
    response.headers.set("x-g2-shutdown", result.shutdown);
    response.headers.set("x-g2-ready", String(result.ready));
    response.headers.set("x-g2-cancelled", String(result.cancelled));
    response.headers.set("x-g2-generation", String(result.generation));
    response.headers.set("x-g2-wasm-memory", String(result.wasm_memory_bytes));
  },
};
export const handleRequest = createHttpHandler(bridgeOptions);
export const responseLimitProof = createHttpHandler({
  ...bridgeOptions,
  maxResponseBodyBytes: 4,
});

// Keep the pending HTTP cancellation closure alive while the generation fails.
export async function recovery(origin) {
  const before = runner.generation();
  const pending = handleRequest(new Request(origin + "/blocked"));
  await new Promise((resolve) => setTimeout(resolve, 10));
  const fault = await runner
    .run(() => trap_probe())
    .then(
      () => false,
      (error) => error.code === "instance_abandoned",
    );
  const failed = await pending;
  const healthy = await handleRequest(new Request(origin + "/method"));
  return {
    passed:
      fault &&
      failed.status === 503 &&
      healthy.status === 200 &&
      healthy.headers.get("x-g2-shutdown") === "clean" &&
      runner.generation() > before,
    before,
    after: runner.generation(),
    peer_status: failed.status,
    healthy_status: healthy.status,
  };
}

export const handleDuplex = createStreamingHttpHandler({
  requestBodyEncoding,
  maxRequestBodyBytes: 65536,
  open: runner.open,
  async openHttp(input, scope) {
    const response = await open_http(input, scope);
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

export async function getCorpus() {
  return (
    await runner.run(() => ({
      shutdown: "clean",
      corpus: JSON.parse(parity_corpus()),
    }))
  ).corpus;
}
