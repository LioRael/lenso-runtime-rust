import { qualify } from "./qualification-matrix.mjs";
export default {
  async test(_controller, env) {
    const request = (url, options) => {
      const service = {
        "http.invalid": env.HTTP,
        "session.invalid": env.SESSION,
        "mixed.invalid": env.MIXED,
      }[new URL(url).hostname];
      if (!service) throw Error("unassigned origin");
      return service.fetch(url, options);
    };
    const socket = async (url) => {
      const response = await request(url, {
        headers: {
          upgrade: "websocket",
          connection: "Upgrade",
          "sec-websocket-version": "13",
          "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ==",
          authorization: "Bearer proof",
          origin: "https://client.invalid",
          "sec-websocket-protocol": "lenso.echo",
        },
      });
      const ws = response.webSocket;
      if (!ws) throw Error(`upgrade failed: ${response.status}`);
      ws.accept();
      return {
        headers: response.headers,
        exchange(value) {
          return new Promise((resolve, reject) => {
            const onMessage = (event) => {
              ws.removeEventListener("message", onMessage);
              if (event.data === value) resolve();
              else reject(Error("echo mismatch"));
            };
            ws.addEventListener("message", onMessage);
            ws.send(value);
          });
        },
        close() {
          return new Promise((resolve) => {
            ws.addEventListener("close", resolve, { once: true });
            ws.close(1000, "complete");
          });
        },
      };
    };
    const evidence = await qualify({
      http: "http://http.invalid",
      session: "http://session.invalid",
      mixed: "http://mixed.invalid",
      fetch: request,
      socket,
    });
    evidence.execution =
      "supplementary workerd test service calls; no external sockets, direct deployment routing or client-disconnect proof";
    console.log("W02_EVIDENCE " + JSON.stringify(evidence));
    if (!evidence.passed) throw Error("W02 supplementary matrix failed");
  },
};
