//! Browser/JavaScript Runtime Driver for the portable Lenso Kernel.

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod browser;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod fallback;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use browser::BrowserDriver;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use fallback::BrowserDriver;
