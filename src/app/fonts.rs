use gpui_kit::{App, Result};
use std::borrow::Cow;

/// Register the same pinned family in production and native previews.
/// Bytes are embedded once; no filesystem reads or per-frame font loading.
pub(super) fn register(cx: &App) -> Result<()> {
    cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-Variable.ttf")),
        Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-Bold.ttf")),
    ])
}
