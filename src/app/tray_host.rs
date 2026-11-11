//! Connects the tray icon to the app: keeps it alive while the window is
//! closed, and turns its menu choices into player and window actions.
use super::*;
use crate::tray::{Tray, TrayCommand};

struct TrayHost {
    // Dropping the tray removes the icon and stops its thread.
    _tray: Tray,
}

impl gpui_kit::Global for TrayHost {}

/// Adds or removes the tray icon. With the icon present, closing the last
/// window no longer quits; the tray menu's Quit does.
pub(super) fn apply(enabled: bool, cx: &mut App) {
    if !enabled {
        if cx.has_global::<TrayHost>() {
            cx.remove_global::<TrayHost>();
        }
        cx.set_quit_mode(gpui_kit::QuitMode::Default);
        return;
    }
    if cx.has_global::<TrayHost>() {
        return;
    }
    match Tray::start() {
        Ok((tray, commands)) => {
            cx.set_quit_mode(gpui_kit::QuitMode::Explicit);
            cx.set_global(TrayHost { _tray: tray });
            cx.spawn(async move |cx| {
                while let Ok(command) = commands.recv().await {
                    cx.update(|cx| handle(command, cx));
                }
            })
            .detach();
        }
        Err(error) => log::warn!("tray: could not add the icon: {error:#}"),
    }
}

fn handle(command: TrayCommand, cx: &mut App) {
    let player = services::AppServices::player(cx);
    match command {
        TrayCommand::Show => {
            cx.activate(true);
            windows::show_app_window(cx);
        }
        TrayCommand::Toggle => player.update(cx, |player, cx| player.toggle(cx)),
        TrayCommand::Next => player.update(cx, |player, cx| player.next(cx)),
        TrayCommand::Previous => player.update(cx, |player, cx| player.previous(cx)),
        TrayCommand::Quit => cx.quit(),
    }
}
