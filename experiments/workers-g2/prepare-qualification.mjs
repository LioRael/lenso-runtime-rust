import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, mkdirSync, readdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { join, relative } from "node:path";
const root = fileURLToPath(new URL(".", import.meta.url));
const hash = (path) =>
  createHash("sha256").update(readFileSync(path)).digest("hex");
const sourceFiles = [];
function inventory(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (
      ["node_modules", "pkg", ".w02", ".wrangler", "evidence"].includes(
        entry.name,
      )
    )
      continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) inventory(path);
    else if (entry.name !== "qualification-identity.json")
      sourceFiles.push([relative(root, path), hash(path)]);
  }
}
inventory(root);
inventory(join(root, "../../packages/workers-runtime"));
sourceFiles.sort(([a], [b]) => a.localeCompare(b));
const identity = {
  schema: "w02-local-identity-v1",
  baseCommit: execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  }).trim(),
  sourceSha256: createHash("sha256")
    .update(JSON.stringify(sourceFiles))
    .digest("hex"),
  sourceFiles,
  wasmSha256: hash(join(root, "pkg/lenso_workers_g2_host_bg.wasm")),
  glueSha256: hash(join(root, "pkg/lenso_workers_g2_host.js")),
  cargoLockSha256: hash(join(root, "Cargo.lock")),
  pnpmLockSha256: hash(join(root, "pnpm-lock.yaml")),
  rust: execFileSync("rustc", ["+1.94.0", "--version"], {
    encoding: "utf8",
  }).trim(),
  bindgen: execFileSync("wasm-bindgen", ["--version"], {
    encoding: "utf8",
  }).trim(),
  workerd: execFileSync(
    join(root, "node_modules/.pnpm/node_modules/.bin/workerd"),
    ["--version"],
    { encoding: "utf8" },
  ).trim(),
  node: process.version,
  wrangler: "4.107.0",
  compatibilityDate: "2026-07-08",
  compatibilityFlags: ["global_fetch_strictly_public", "enable_request_signal"],
  scope:
    "local-only; no Cloudflare production, external IdP, product capacity, CPU isolation or memory-peak claim",
};
writeFileSync(
  join(root, "qualification-identity.json"),
  JSON.stringify(identity, null, 2) + "\n",
);
mkdirSync(join(root, ".w02"), { recursive: true });
for (const profile of ["http", "session", "mixed"]) {
  execFileSync(
    join(root, "node_modules/.pnpm/node_modules/.bin/esbuild"),
    [
      `profile-${profile}.mjs`,
      "--bundle",
      "--format=esm",
      "--external:*.wasm",
      `--outfile=.w02/${profile}.mjs`,
      `--metafile=.w02/${profile}-modules.json`,
    ],
    { cwd: root, stdio: "inherit" },
  );
  const meta = JSON.parse(
    readFileSync(join(root, `.w02/${profile}-modules.json`)),
  );
  for (const suffix of [
    "pkg/lenso_workers_g2_host.js",
    "/clock.mjs",
    "/runner.mjs",
  ]) {
    const matching = Object.keys(meta.inputs).filter((path) =>
      path.endsWith(suffix),
    );
    if (matching.length !== 1)
      throw Error(`${profile}: expected one ${suffix}: ${matching}`);
  }
}
console.log(
  JSON.stringify({
    prepared: true,
    wasm: identity.wasmSha256,
    source: identity.sourceSha256,
  }),
);
