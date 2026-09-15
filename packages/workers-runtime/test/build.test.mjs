import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

test("pnpm-style symlink invokes the CLI instead of silently succeeding", () => {
  const directory = mkdtempSync(join(tmpdir(), "workers-build-"));
  try {
    const entry = join(directory, "lenso-workers-build.mjs");
    symlinkSync(fileURLToPath(new URL("../build.mjs", import.meta.url)), entry);
    const result = spawnSync(process.execPath, [entry], { encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /manifest, package, and out-dir are required/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
