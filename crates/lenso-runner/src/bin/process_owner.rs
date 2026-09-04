//! Private native ownership helper, not an application runtime or public CLI.

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "../process_owner/mod.rs"]
mod owner;

fn main() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Err(error) = owner::run() {
        eprintln!("process owner failed: {error}");
        std::process::exit(1);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        eprintln!("native process ownership is supported only on macOS and Linux");
        std::process::exit(1);
    }
}
