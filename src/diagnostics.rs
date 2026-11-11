//! Capture failures before GPUI creates a window in the Windows GUI executable.
use std::{backtrace::Backtrace, fs, path::PathBuf, sync::OnceLock};

use directories::ProjectDirs;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn init() {
    let directory = ProjectDirs::from("dev", "twoducks", "cadence")
        .map(|dirs| dirs.data_local_dir().join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("cadence-logs"));
    let path = directory.join("cadence.log");
    let result = (|| -> std::io::Result<_> {
        fs::create_dir_all(directory)?;
        crate::rotating_log::RotatingLog::open(path.clone(), 5 * 1024 * 1024)
    })();
    if let Ok(file) = result
        && env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .format_timestamp_millis()
            .target(env_logger::Target::Pipe(Box::new(file)))
            .try_init()
            .is_ok()
    {
        let _ = LOG_PATH.set(path);
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log::error!("fatal panic: {panic}\n{}", Backtrace::force_capture());
        log::logger().flush();
        if std::thread::current().name() == Some("main") {
            show_error(&format!("Sonder could not start:\n\n{panic}"));
        }
        previous(panic);
    }));
    log::info!(
        "startup: Sonder {} ({}) pid={} executable={:?}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::process::id(),
        std::env::current_exe().ok(),
    );
}

pub(crate) fn show_error(message: &str) {
    use windows_sys::{Win32::UI::WindowsAndMessaging::*, core::w};
    let location = LOG_PATH.get().map_or_else(
        || "No diagnostic log could be created.".to_owned(),
        |path| format!("Diagnostic log: {}", path.display()),
    );
    let message: Vec<u16> = format!("{message}\n\n{location}")
        .replace('\0', "")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // Give the dialog its own message pump. Pumping Windows messages while
    // GPUI holds an App borrow could otherwise re-enter its event handlers.
    if let Ok(dialog) = std::thread::Builder::new()
        .name("cadence-error-dialog".into())
        .spawn(move || {
            // SAFETY: the message and title stay NUL-terminated and valid for
            // the synchronous call. A null owner is valid before UI creation.
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    w!("Sonder startup error"),
                    MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
                );
            }
        })
    {
        let _ = dialog.join();
    }
}
