#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { mkdirSync } from "node:fs";

// Paths come from the consumer, never a sibling repository's checkout layout.
export function build({
  manifest,
  packageName,
  outDir,
  cargo = process.env.CARGO || "cargo",
  cargoConfig,
  bindgen = process.env.WASM_BINDGEN || "wasm-bindgen",
  features = [],
}) {
  if (!manifest || !packageName || !outDir)
    throw new Error("manifest, package, and out-dir are required");
  const version = spawnSync(bindgen, ["--version"], { encoding: "utf8" });
  if (
    version.status !== 0 ||
    version.stdout.trim() !== "wasm-bindgen 0.2.127"
  ) {
    throw new Error(
      "Install wasm-bindgen-cli 0.2.127, matching the Workers Driver lock",
    );
  }
  const command = [
    "+1.94.0",
    "rustc",
    "--locked",
    ...(cargoConfig ? ["--config", resolve(cargoConfig)] : []),
    "--manifest-path",
    resolve(manifest),
    "--target",
    "wasm32-unknown-unknown",
    "-p",
    packageName,
    "--release",
    "--no-default-features",
    "--message-format=json",
    ...(features.length ? ["--features", features.join(",")] : []),
    "--",
    "-C",
    "link-arg=--export=__wasm_call_ctors",
  ];
  const result = spawnSync(cargo, command, {
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    stdio: ["inherit", "pipe", "inherit"],
  });
  const messages = (result.stdout || "")
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  for (const message of messages) {
    if (message.reason === "compiler-message")
      process.stderr.write(message.message.rendered || message.message.message);
  }
  if (result.status !== 0)
    throw result.error || new Error(`Cargo failed (${result.status})`);
  const artifacts = messages
    .filter(
      (message) =>
        message.reason === "compiler-artifact" &&
        message.target.name === packageName.replaceAll("-", "_"),
    )
    .flatMap((message) => message.filenames)
    .filter((name) => name.endsWith(".wasm"));
  if (artifacts.length !== 1)
    throw new Error("Expected exactly one Wasm host artifact");
  mkdirSync(resolve(outDir), { recursive: true });
  const generated = spawnSync(
    bindgen,
    [
      artifacts[0],
      "--target",
      "web",
      "--experimental-reset-state-function",
      "--out-dir",
      resolve(outDir),
    ],
    { stdio: "inherit" },
  );
  if (generated.status !== 0)
    throw generated.error || new Error("wasm-bindgen failed");
  return artifacts[0];
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    const options = {},
      names = {
        "--manifest": "manifest",
        "--package": "packageName",
        "--out-dir": "outDir",
        "--features": "features",
        "--config": "cargoConfig",
      };
    for (let index = 2; index < process.argv.length; index += 2) {
      const key = names[process.argv[index]],
        value = process.argv[index + 1];
      if (!key || !value || value.startsWith("--"))
        throw new Error(
          "Usage: lenso-workers-build --manifest Cargo.toml --package host --out-dir pkg [--features a,b]",
        );
      if (key in options)
        throw new Error(`Duplicate option ${process.argv[index]}`);
      options[key] = key === "features" ? value.split(",") : value;
    }
    build(options);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
