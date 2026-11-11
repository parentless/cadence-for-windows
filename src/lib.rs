mod app;
mod audio_output;
pub mod backend;
mod credential_worker;
#[cfg(target_os = "windows")]
mod diagnostics;
pub(crate) mod discord;
pub(crate) mod lastfm;
pub mod lifecycle;
pub mod model;
mod oauth_callback;
mod oauth_page;
pub mod playback;
#[cfg(any(target_os = "windows", test))]
mod rotating_log;
pub mod spotify;
pub mod storage;

pub use app::run;

#[cfg(any(target_os = "windows", test))]
mod update_release;
