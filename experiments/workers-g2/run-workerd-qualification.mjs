import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
const root = fileURLToPath(new URL(".", import.meta.url));
const output = resolve(
  process.argv[2] || "docs/evidence/workers-w02/workerd-test.json.gz",
);
execFileSync(process.execPath, ["prepare-qualification.mjs"], {
  cwd: root,
  stdio: "inherit",
});
const bin = root + "node_modules/.pnpm/node_modules/.bin/";
execFileSync(
  bin + "esbuild",
  [
    "qualification-workerd-test.mjs",
    "--bundle",
    "--format=esm",
    "--outfile=.w02/test.mjs",
  ],
  { cwd: root, stdio: "inherit" },
);
const args = [
  "test",
  "-I",
  "node_modules/.pnpm/workerd@1.20260701.1/node_modules",
  "qualification.capnp",
];
const startedAt = new Date().toISOString();
const run = spawnSync(bin + "workerd", args, {
  cwd: root,
  encoding: "utf8",
  timeout: 45000,
  maxBuffer: 16 * 1024 * 1024,
});
const log = (run.stdout || "") + (run.stderr || "");
const line = log.split("\n").find((line) => line.startsWith("W02_EVIDENCE "));
const evidence = line
  ? JSON.parse(line.slice(13))
  : {
      passed: false,
      status: "missing-required-evidence",
      error: String(run.error || log),
      cases: [],
    };
Object.assign(evidence, {
  startedAt,
  finishedAt: new Date().toISOString(),
  command: ["workerd", ...args],
  exitCode: run.status,
  warningLines: log
    .split("\n")
    .filter((line) => /Warning:|uncaught exception/.test(line)),
  artifact: JSON.parse(readFileSync(root + "qualification-identity.json")),
  bundles: Object.fromEntries(
    ["http", "session", "mixed"].map((profile) => [
      profile,
      JSON.parse(readFileSync(root + `.w02/${profile}-modules.json`)),
    ]),
  ),
});
if (run.status !== 0) evidence.passed = false;
mkdirSync(dirname(output), { recursive: true });
writeFileSync(output, gzipSync(JSON.stringify(evidence, null, 2) + "\n", { level: 9 }));
console.log(
  JSON.stringify({
    passed: evidence.passed,
    count: evidence.cases?.length,
    shortRequests: evidence.successfulShortRequests,
    output,
  }),
);
if (!evidence.passed) process.exitCode = 1;
