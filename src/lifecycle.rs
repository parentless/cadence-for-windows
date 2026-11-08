//! Single-instance activation using the platform's local IPC facilities.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

// Exercise the portable lock/loopback protocol on macOS too; Windows CI also
// runs it with Windows' own file-lock and socket implementations.
#[cfg(all(test, unix))]
#[path = "lifecycle/windows.rs"]
#[allow(dead_code)]
mod windows_protocol_tests;
