import * as bindings from "./pkg/lenso_workers_g2_host.js";
import module from "./pkg/lenso_workers_g2_host_bg.wasm";
import { clearTimers } from "./clock.mjs";
import { requestBodyQualification } from "./request-body-qualification.mjs";

const cases = [];
requestBodyQualification((name, options, run) => {
  cases.push({ name, run: run ?? options, timeout: options.timeout ?? 30000 });
}, bindings, module, clearTimers);

export default {
  async test() {
    for (const { name, run, timeout } of cases) {
      let timer;
      try {
        await Promise.race([
          run(),
          new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error(`${name}: timed out`)), timeout);
          }),
        ]);
        console.log(`PASS ${name}`);
      } finally {
        clearTimeout(timer);
      }
    }
    console.log(`PASS ${cases.length} real Rust/Wasm request-body qualifications`);
  },
};
