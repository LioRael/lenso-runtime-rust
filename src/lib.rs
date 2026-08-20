//! WASI Preview 2 Runtime Driver for the portable Lenso Kernel.

#[cfg(not(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2")))]
mod fallback;
#[cfg(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"))]
mod wasip2;

#[cfg(not(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2")))]
pub use fallback::WasiDriver;
#[cfg(all(target_arch = "wasm32", target_os = "wasi", target_env = "p2"))]
pub use wasip2::WasiDriver;
