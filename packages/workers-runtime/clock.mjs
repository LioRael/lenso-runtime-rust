// One generated wasm-bindgen module owns this timer domain. Its runner must use
// this same clearTimers export before resetting that module's state.
const timers = new Map();
let nextId = 0;
export function clock(operation, callback, value) {
  if (operation === 0) return performance.now();
  if (operation === 1) {
    do {
      nextId = nextId === 0x7fffffff ? 1 : nextId + 1;
    } while (timers.has(nextId));
    const id = nextId;
    timers.set(
      id,
      setTimeout(() => {
        timers.delete(id);
        callback();
      }, value),
    );
    return id;
  }
  if (operation === 2) {
    clearTimeout(timers.get(value));
    timers.delete(value);
    return 0;
  }
  throw new Error("Unknown clock operation");
}
export function clearTimers() {
  for (const timer of timers.values()) clearTimeout(timer);
  timers.clear();
}
