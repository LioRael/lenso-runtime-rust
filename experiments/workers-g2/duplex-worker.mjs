import { handleDuplex } from "./runner.mjs";
export default { fetch: (request) => handleDuplex(request) };
