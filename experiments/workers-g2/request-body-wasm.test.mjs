// Missing generated artifacts fail this suite; no mocks or skipped qualification.
import { readFile } from "node:fs/promises";
import test from "node:test";
import * as bindings from "./pkg/lenso_workers_g2_host.js";
import { clearTimers } from "./clock.mjs";
import { requestBodyQualification } from "./request-body-qualification.mjs";

const module = new WebAssembly.Module(await readFile(new URL("./pkg/lenso_workers_g2_host_bg.wasm", import.meta.url)));
requestBodyQualification(test, bindings, module, clearTimers);
