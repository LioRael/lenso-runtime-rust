// JS timer handles must not retain callbacks belonging to an abandoned Wasm VM.
const timers = new Set();
export function clock(operation, callback, value) {
  if (operation === 0) return performance.now();
  if (operation === 1) {
    const id = setTimeout(() => { timers.delete(id); callback(); }, value);
    timers.add(id);
    return id;
  }
  if (operation === 2) { clearTimeout(value); timers.delete(value); return 0; }
  throw new Error('Unknown clock operation');
}
export function clearTimers() {
  for (const id of timers) clearTimeout(id);
  timers.clear();
}
