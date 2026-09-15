using Workerd = import "/workerd/workerd.capnp";
const config :Workerd.Config = (
  services = [
    (name = "http", worker = .http),
    (name = "session", worker = .session),
    (name = "mixed", worker = .mixed),
    (name = "matrix", worker = (
      modules = [(name = "test.mjs", esModule = embed ".w02/test.mjs")],
      compatibilityDate = "2026-07-08",
      bindings = [(name = "HTTP", service = "http"), (name = "SESSION", service = "session"), (name = "MIXED", service = "mixed")]
    ))
  ]
);
const http :Workerd.Worker = (
  modules = [(name = "main.mjs", esModule = embed ".w02/http.mjs"),
    (name = "pkg/lenso_workers_g2_host_bg.wasm", wasm = embed "pkg/lenso_workers_g2_host_bg.wasm")],
  compatibilityDate = "2026-07-08", compatibilityFlags = ["global_fetch_strictly_public", "enable_request_signal"]
);
const session :Workerd.Worker = (
  modules = [(name = "main.mjs", esModule = embed ".w02/session.mjs"),
    (name = "pkg/lenso_workers_g2_host_bg.wasm", wasm = embed "pkg/lenso_workers_g2_host_bg.wasm")],
  compatibilityDate = "2026-07-08", compatibilityFlags = ["global_fetch_strictly_public", "enable_request_signal"]
);
const mixed :Workerd.Worker = (
  modules = [(name = "main.mjs", esModule = embed ".w02/mixed.mjs"),
    (name = "pkg/lenso_workers_g2_host_bg.wasm", wasm = embed "pkg/lenso_workers_g2_host_bg.wasm")],
  compatibilityDate = "2026-07-08", compatibilityFlags = ["global_fetch_strictly_public", "enable_request_signal"]
);
