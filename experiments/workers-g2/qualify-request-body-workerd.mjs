// Bundle the same assertions used by Node, then execute workerd's native test
// entry point. This requires no listening socket or external service.
import { createRequire } from "node:module";
import { writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
const root = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const requireWrangler = createRequire(require.resolve("wrangler/package.json"));
const { build } = requireWrangler("esbuild");
const workerd = requireWrangler.resolve("workerd/bin/workerd");
await build({
  entryPoints: [resolve(root, "request-body-workerd.mjs")],
  outfile: resolve(root, "pkg/request-body-workerd.mjs"),
  bundle: true,
  format: "esm",
  platform: "neutral",
  external: ["node:*"],
  plugins: [{ name: "wasm-module", setup(build) {
    build.onResolve({ filter: /\.wasm$/ }, () => ({ path: "g2.wasm", external: true }));
  } }],
});
await writeFile(resolve(root, "pkg/request-body-workerd.capnp"), `
using Workerd = import "/workerd/workerd.capnp";
const config :Workerd.Config = (
  services = [(name = "request-body", worker = (
    modules = [
      (name = "qualification.mjs", esModule = embed "request-body-workerd.mjs"),
      (name = "g2.wasm", wasm = embed "lenso_workers_g2_host_bg.wasm")
    ],
    compatibilityDate = "2026-07-08",
    compatibilityFlags = ["global_fetch_strictly_public", "enable_request_signal", "nodejs_compat"]
  ))]
);
`);
const result = spawnSync(workerd, ["test", resolve(root, "pkg/request-body-workerd.capnp")], { stdio: "inherit" });
if (result.error) throw result.error;
if (result.status !== 0) process.exitCode = result.status ?? 1;
