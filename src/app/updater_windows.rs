//! Windows releases use the same repository's portable executable and checksum.
//! A short-lived helper replaces the executable after orderly app shutdown.
use super::*;
use crate::update_release::Release;
use anyhow::{Context as _, Result, ensure};
use directories::ProjectDirs;
use std::{fs, path::PathBuf};

#[derive(Default)]
pub(super) struct UpdateStatus {
    pub(super) check: UpdateCheck,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum UpdateCheck {
    #[default]
    Unknown,
    Checking,
    Downloading,
    UpToDate,
    Available(String),
    #[allow(dead_code)] // Shared settings UI also displays Sparkle's skipped versions.
    Skipped(String),
    Failed(String),
}
struct Updater {
    status: Entity<UpdateStatus>,
    release: Option<Release>,
    automatic: bool,
    busy: bool,
    _timer: gpui_kit::Task<()>,
}
impl gpui_kit::Global for Updater {}

fn preference_path() -> Option<PathBuf> {
    Some(
        ProjectDirs::from("dev", "twoducks", "cadence")?
            .config_dir()
            .join("automatic-updates.json"),
    )
}

pub(super) fn start(cx: &mut App) -> bool {
    // Source builds stay inert. Release CI pins the feed to the fork producing
    // this executable, rather than silently fetching upstream's macOS releases.
    if cfg!(debug_assertions) || option_env!("CADENCE_UPDATE_REPOSITORY").is_none() {
        return false;
    }
    let automatic = preference_path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(true);
    let status = cx.new(|_| UpdateStatus::default());
    let timer = cx.spawn(async move |cx| {
        let mut delay = Duration::from_secs(15);
        loop {
            cx.background_executor().timer(delay).await;
            cx.update(|cx| {
                if cx
                    .try_global::<Updater>()
                    .is_some_and(|u| u.automatic && !u.busy)
                {
                    check(cx, false);
                }
            });
            delay = Duration::from_secs(24 * 60 * 60);
        }
    });
    cx.set_global(Updater {
        status,
        release: None,
        automatic,
        busy: false,
        _timer: timer,
    });
    true
}

pub(super) fn status(cx: &App) -> Option<Entity<UpdateStatus>> {
    cx.try_global::<Updater>().map(|u| u.status.clone())
}
pub(super) fn automatic_checks(cx: &App) -> Option<bool> {
    cx.try_global::<Updater>().map(|u| u.automatic)
}
pub(super) fn set_automatic_checks(enabled: bool, cx: &mut App) {
    if let Some(path) = preference_path() {
        let result = (|| -> Result<()> {
            fs::create_dir_all(path.parent().context("missing config directory")?)?;
            fs::write(path, serde_json::to_vec(&enabled)?)?;
            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("could not save update preference: {error}");
        }
    }
    if cx.has_global::<Updater>() {
        cx.global_mut::<Updater>().automatic = enabled;
    }
}
pub(super) fn check_for_updates(cx: &mut App) {
    check(cx, true);
}

fn check(cx: &mut App, install_if_available: bool) {
    if !cx.has_global::<Updater>() {
        return;
    }
    let updater = cx.global_mut::<Updater>();
    if updater.busy {
        return;
    }
    updater.busy = true;
    let release = if install_if_available {
        updater.release.clone()
    } else {
        None
    };
    let installing = release.is_some();
    let status = updater.status.clone();
    status.update(cx, |status, cx| {
        status.check = if installing {
            UpdateCheck::Downloading
        } else {
            UpdateCheck::Checking
        };
        cx.notify();
    });
    let (sender, receiver) = async_channel::bounded(1);
    // HTTP and disk work never runs on the UI or audio thread.
    std::thread::spawn(move || {
        let result = (|| -> Result<Option<Release>> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let client = reqwest::Client::builder()
                    .user_agent("Cadence Windows updater")
                    .timeout(Duration::from_secs(180))
                    .build()?;
                if let Some(release) = release {
                    stage_update(&client, &release).await?;
                    Ok(None)
                } else {
                    latest_release(&client).await
                }
            })
        })();
        let _ = sender.send_blocking(result);
    });
    cx.spawn(async move |cx| {
        if let Ok(result) = receiver.recv().await {
            cx.update(|cx| {
                let updater = cx.global_mut::<Updater>();
                updater.busy = false;
                let check = match result {
                    Ok(_) if installing => {
                        cx.quit();
                        return;
                    }
                    Ok(release) => {
                        let check = release.as_ref().map_or(UpdateCheck::UpToDate, |r| {
                            UpdateCheck::Available(r.version.clone())
                        });
                        updater.release = release;
                        check
                    }
                    Err(error) => {
                        log::warn!("update failed: {error:#}");
                        updater.release = None;
                        UpdateCheck::Failed(error.to_string())
                    }
                };
                let status = updater.status.clone();
                status.update(cx, |status, cx| {
                    status.check = check;
                    cx.notify();
                });
            });
        }
    })
    .detach();
}

async fn latest_release(client: &reqwest::Client) -> Result<Option<Release>> {
    let repository =
        option_env!("CADENCE_UPDATE_REPOSITORY").context("update feed is not configured")?;
    crate::update_release::latest_release(client, repository, env!("CARGO_PKG_VERSION")).await
}

async fn stage_update(client: &reqwest::Client, release: &Release) -> Result<()> {
    use futures::StreamExt as _;
    use sha2::{Digest, Sha256};
    use std::{io::Write as _, os::windows::process::CommandExt as _, process::Command};
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .context("missing executable directory")?;
    let staged = directory.join(format!("cadence-update-{}.exe", std::process::id()));
    let result = async {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .context(
                "cannot write next to Cadence; move it to a writable folder before updating",
            )?;
        let mut response = client
            .get(&release.url)
            .send()
            .await?
            .error_for_status()?
            .bytes_stream();
        let mut digest = Sha256::new();
        let mut size = 0usize;
        while let Some(chunk) = response.next().await {
            let chunk = chunk?;
            size += chunk.len();
            ensure!(size <= 300 * 1024 * 1024, "update exceeds size limit");
            file.write_all(&chunk)?;
            digest.update(&chunk);
        }
        file.sync_all()?;
        ensure!(
            format!("{:x}", digest.finalize()).eq_ignore_ascii_case(&release.checksum),
            "update checksum mismatch"
        );
        drop(file);
        let helper = directory.join(format!("cadence-update-{}.ps1", std::process::id()));
        fs::write(
            &helper,
            include_str!("../../scripts/install-windows-update.ps1"),
        )?;
        Command::new("powershell.exe")
            .creation_flags(0x08000000)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&helper)
            .arg("-ProcessId")
            .arg(std::process::id().to_string())
            .arg("-Executable")
            .arg(&executable)
            .arg("-Staged")
            .arg(&staged)
            .spawn()?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}
