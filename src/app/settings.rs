use super::*;
use gpui_kit::TestSupportExt as _;

#[derive(Clone)]
struct MascotOption {
    label: &'static str,
    preference: MascotPreference,
}

impl SelectItem for MascotOption {
    type Value = MascotPreference;

    fn title(&self) -> SharedString {
        self.label.into()
    }

    fn value(&self) -> &Self::Value {
        &self.preference
    }
}

fn mascot_options() -> Vec<MascotOption> {
    vec![
        MascotOption {
            label: "None",
            preference: MascotPreference::None,
        },
        MascotOption {
            label: "Vespa Romeo",
            preference: MascotPreference::RomeoVespa,
        },
        MascotOption {
            label: "Vespa Duo",
            preference: MascotPreference::VespaDuo,
        },
        MascotOption {
            label: "Tarantella Dancer",
            preference: MascotPreference::TarantellaDancer,
        },
    ]
}

/// What the settings page asks the workspace to do.
pub(super) enum SettingsEvent {
    RequestAppChange,
    SetTheme(ThemePreference),
    SetZoom(UiZoom),
    SetAutoplay(bool),
    SetMascot(MascotPreference),
    SetDiscord(crate::discord::Config),
    SetLastfm(crate::lastfm::Config),
}

/// Broad groups, so the page stays short without a tab for every setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsTab {
    /// Appearance and playback.
    General,
    /// Spotify, Discord and Last.fm.
    Connections,
    /// Updates and cache.
    System,
}

impl SettingsTab {
    const ALL: [Self; 3] = [Self::General, Self::Connections, Self::System];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Connections => "Connections",
            Self::System => "System",
        }
    }
}

/// The settings page: appearance, playback, and the Spotify developer app.
pub(super) struct Settings {
    session: Entity<session::Session>,
    tab: SettingsTab,
    scroll: gpui_kit::ScrollHandle,
    mascot_select: Entity<SelectState<Vec<MascotOption>>>,
    /// Each switch is ringed while its focus is inside, in place of the kit's
    /// ring, which also shows after a click.
    autoplay_focus: FocusHandle,
    automatic_updates_focus: FocusHandle,
    discord_focus: FocusHandle,
    discord_application_id: Entity<InputState>,
    discord_error: Option<SharedString>,
    lastfm_focus: FocusHandle,
    lastfm_api_key: Entity<InputState>,
    lastfm_api_secret: Entity<InputState>,
    lastfm_status: Option<SharedString>,
    /// Set between asking Last.fm for a sign-in link and finishing it.
    lastfm_token: Option<String>,
    lastfm_busy: bool,
    cache_cleared: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for Settings {}

impl Settings {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let discord_id = services::AppServices::discord_config(cx)
            .application_id
            .clone();
        let discord_application_id = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Discord Application ID");
            input.set_value(discord_id, window, cx);
            input
        });
        let mascot = services::AppServices::preferences(cx).mascot;
        let mascot_select = cx.new(|cx| {
            SelectState::new(
                mascot_options(),
                Some(IndexPath::default().row(mascot_index(mascot))),
                window,
                cx,
            )
        });
        let mut subscriptions = vec![cx.subscribe_in(
            &mascot_select,
            window,
            |_, _, event: &SelectEvent<Vec<MascotOption>>, _, cx| {
                let SelectEvent::Confirm(Some(mascot)) = event else {
                    return;
                };
                cx.emit(SettingsEvent::SetMascot(*mascot));
            },
        )];
        subscriptions.push(cx.subscribe(
            &discord_application_id,
            |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    this.discord_error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    let enabled = services::AppServices::discord_config(cx).enabled;
                    this.save_discord(enabled, cx);
                }
                _ => {}
            },
        ));
        let lastfm_api_key = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Last.fm API key");
            input.set_value(
                services::AppServices::lastfm_config(cx).api_key.clone(),
                window,
                cx,
            );
            input
        });
        let lastfm_api_secret =
            cx.new(|cx| InputState::new(window, cx).placeholder("Last.fm shared secret"));
        if let Some(status) = updater::status(cx) {
            subscriptions.push(cx.observe(&status, |_, _, cx| cx.notify()));
        }
        Self {
            session: services::AppServices::session(cx),
            tab: SettingsTab::General,
            scroll: gpui_kit::ScrollHandle::new(),
            mascot_select,
            autoplay_focus: cx.focus_handle(),
            automatic_updates_focus: cx.focus_handle(),
            discord_focus: cx.focus_handle(),
            lastfm_focus: cx.focus_handle(),
            lastfm_api_key,
            lastfm_api_secret,
            lastfm_status: None,
            lastfm_token: None,
            lastfm_busy: false,
            discord_application_id,
            discord_error: None,
            cache_cleared: false,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn sync_mascot(
        &mut self,
        mascot: MascotPreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mascot_select.update(cx, |select, cx| {
            select.set_selected_index(
                Some(IndexPath::default().row(mascot_index(mascot))),
                window,
                cx,
            );
        });
    }

    #[cfg(test)]
    pub(super) fn autoplay_focus(&self) -> &FocusHandle {
        &self.autoplay_focus
    }

    fn page(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let session = self.session.read(cx);
        let saved_configuration = session.client_id_source() == Some(ClientIdSource::Saved);
        let environment_configuration =
            session.client_id_source() == Some(ClientIdSource::Environment);
        let client_id = session.client_id().cloned().unwrap_or_default();
        let dashboard_url = format!("{}/{client_id}", onboarding::SPOTIFY_DASHBOARD_URL);
        let preferences = services::AppServices::preferences(cx);
        let autoplay = preferences.autoplay;

        let tab = self.tab;
        // The tab list stays put while the selected group scrolls beside it.
        div()
            .size_full()
            .flex()
            .justify_center()
            .child(self.tab_list(palette, cx))
            .child(
        div()
            .id("settings-page").test_support()
            .h_full()
            .flex_1()
            .min_w_0()
            .max_w(tokens::SETTINGS_MAX_WIDTH)
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .pr_10()
                    .pt_7()
                    .pb_16()
                    .when(tab == SettingsTab::General, |content| content
                    .child(
                        div()
                            .child(Self::settings_section_header(
                                palette,
                                "Appearance",
                                "Choose how Cadence looks.",
                            ))
                            .child(
                                div()
                                    .mt_4()
                                    .p_1()
                                    .rounded(tokens::CONTAINER_RADIUS)
                                    .bg(rgb(palette.control))
                                    .flex()
                                    .gap_1()
                                    .child(self.appearance_option(
                                        "settings-appearance-system",
                                        CadenceIcon::SystemAppearance,
                                        "System",
                                        ThemePreference::System,
                                        cx,
                                    ))
                                    .child(self.appearance_option(
                                        "settings-appearance-light",
                                        CadenceIcon::Sun,
                                        "Light",
                                        ThemePreference::Light,
                                        cx,
                                    ))
                                    .child(self.appearance_option(
                                        "settings-appearance-dark",
                                        CadenceIcon::Moon,
                                        "Dark",
                                        ThemePreference::Dark,
                                        cx,
                                    )),
                            ),
                    )
                    .child(div().mt_5().child(components::section_label(palette, "UI zoom"))
                        .child(div().mt_2().flex().flex_wrap().gap_2().children(UiZoom::ALL.into_iter().map(|zoom| {
                            components::button(palette, ("settings-ui-zoom", usize::from(zoom.percent())))
                                .test_support().h_9().px_3().rounded_lg()
                                .bg(rgb(if preferences.ui_zoom == zoom { palette.selection } else { palette.control }))
                                .hover(|style| style.bg(rgb(palette.control_hover)))
                                .child(format!("{}%", zoom.percent()))
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(SettingsEvent::SetZoom(zoom))))
                        })))
                    )
                    .child(
                        div().mt_5().child(components::section_label(palette, "Selection color"))
                            .child(div().mt_2().flex().flex_wrap().gap_2().children(
                                SelectionColor::ALL.into_iter().enumerate().map(|(index, color)| {
                                    components::button(palette, ("settings-selection-color", index))
                                        .hover(|style| style.opacity(0.9))
                                        .test_support().h_9().px_3().rounded_lg()
                                        .bg(rgb(color.fill(palette.canvas == CadencePalette::DARK.canvas)))
                                        .border_1().border_color(rgb(if preferences.selection_color == color { palette.text_primary } else { palette.border }))
                                        .child(color.name())
                                        .on_click(move |_, window, cx| appearance::Appearance::set_selection_color(color, window, cx))
                                })
                            ))
                    )
                    .child(
                        div()
                            .mt_12()
                            .child(Self::settings_section_header(
                                palette,
                                "Playback",
                                "Control playback behavior and player-bar extras.",
                            ))
                            .child(
                                div()
                                    .mt_4()
                                    .rounded_2xl()
                                    .border_1()
                                    .border_color(rgb(palette.border))
                                    .bg(rgb(palette.surface_raised))
                                    .p_5()
                                    .child(Self::playback_setting_row(
                                        palette,
                                        "Autoplay",
                                        "Keep the music going with similar songs when the queue ends.",
                                        components::focus_ring_around(
                                            palette,
                                            &self.autoplay_focus,
                                            window,
                                            cx,
                                            Switch::new("settings-autoplay")
                                                .focus_ring(false)
                                                .checked(autoplay)
                                                .on_click(cx.listener(
                                                    |_, checked: &bool, _, cx| {
                                                        cx.emit(SettingsEvent::SetAutoplay(
                                                            *checked,
                                                        ));
                                                    },
                                                )),
                                        ),
                                    ))
                                    .child(
                                        div()
                                            .mt_5()
                                            .pt_5()
                                            .border_t_1()
                                            .border_color(rgb(palette.border))
                                            .child(Self::playback_setting_row(
                                                palette,
                                                "Mascot",
                                                "Choose which mascot appears above the player bar.",
                                                Select::new(&self.mascot_select)
                                                    .id("settings-mascot")
                                                    .w(tokens::SETTINGS_FIELD_WIDTH)
                                                    .menu_width(tokens::SETTINGS_FIELD_WIDTH),
                                            )),
                                    ),
                            ),
                    )
                    )
                    .when(tab == SettingsTab::Connections, |content| content
                    .child(
                        div()
                            .child(Self::settings_section_header(
                                palette,
                                "Spotify",
                                "Manage the developer app Cadence uses to connect to Spotify.",
                            ))
                            .child(
                                div()
                                    .mt_4()
                                    .rounded_2xl()
                                    .border_1()
                                    .border_color(rgb(palette.border))
                                    .bg(rgb(palette.surface_raised))
                                    .p_5()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_5()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_weight(
                                                                gpui_kit::FontWeight::MEDIUM,
                                                            )
                                                            .text_color(rgb(palette.text))
                                                            .child(if environment_configuration {
                                                                "Configured by SPOTIFY_CLIENT_ID"
                                                            } else {
                                                                "Spotify developer app"
                                                            }),
                                                    )
                                                    .child(
                                                        components::flush_button(palette,
                                                            "settings-open-spotify-dashboard",
                                                        )
                                                            .mt_1p5()
                                                            .rounded_md()
                                                            .text_sm()
                                                            .text_color(rgb(palette.link))
                                                            .underline()
                                                            .gap(tokens::INLINE_ICON_GAP)
                                                            .justify_start()
                                                            .child(client_id)
                                                            .child(components::icon(
                                                                CadenceIcon::ArrowUpRight,
                                                                tokens::INLINE_ICON,
                                                                palette.link,
                                                            ))
                                                            .on_click(cx.listener(
                                                                move |_, _, _, cx| {
                                                                    cx.open_url(&dashboard_url);
                                                                },
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_none()
                                                    .gap_2()
                                                    .when(
                                                        saved_configuration,
                                                        |actions| {
                                                            actions.child(
                                                                components::settings_button(palette,
                                                                    "settings-change-spotify-app",
                                                                    "Change developer app",
                                                                )
                                                                .on_click(cx.listener(
                                                                    |_, _, _, cx| {
                                                                        cx.emit(
                                                                            SettingsEvent::RequestAppChange,
                                                                        );
                                                                    },
                                                                )),
                                                            )
                                                        },
                                                    ),
                                            ),
                                    )
                                    .when(environment_configuration, |card| {
                                        card.child(
                                            div()
                                                .mt_3()
                                                .text_size(tokens::BODY_TEXT)
                                                .line_height(relative(1.45))
                                                .text_color(rgb(palette.text))
                                                .child(
                                                    "Quit Cadence and update the environment variable to change the developer app.",
                                                ),
                                        )
                                    }),
                            ),
                    )
                    .child(self.discord_section(palette, window, cx))
                    .child(self.lastfm_section(palette, window, cx))
                    )
                    .when(tab == SettingsTab::System, |content| content
                    .child(self.updates_section(palette, window, cx).mt_0())
                    .child(self.cache_section(palette, cx))),
            )
            )
    }

    fn tab_list(&self, palette: CadencePalette, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .w(tokens::SETTINGS_NAV_WIDTH)
            .mr_6()
            .pl_10()
            .pt_7()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                SettingsTab::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        let selected = self.tab == tab;
                        components::filled_button(palette, ("settings-tab", index))
                            .test_support()
                            .h_10()
                            .w_full()
                            .px_3()
                            .justify_start()
                            .rounded(tokens::CONTROL_RADIUS)
                            .text_sm()
                            .font_weight(if selected {
                                gpui_kit::FontWeight::SEMIBOLD
                            } else {
                                gpui_kit::FontWeight::MEDIUM
                            })
                            .text_color(rgb(palette.text_primary))
                            .when(selected, |button| button.bg(rgb(palette.selection)))
                            .hover(|style| style.bg(rgb(palette.control_hover)))
                            .child(tab.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.tab != tab {
                                    this.tab = tab;
                                    // Each group starts at its top.
                                    this.scroll.set_offset(gpui_kit::point(
                                        gpui_kit::px(0.),
                                        gpui_kit::px(0.),
                                    ));
                                    cx.notify();
                                }
                            }))
                    }),
            )
    }

    fn save_discord(&mut self, enabled: bool, cx: &mut Context<Self>) {
        // Turning sharing off must always work, even if the field contains
        // an unfinished edit. Keep the saved ID when disabling the toggle.
        let saved = services::AppServices::discord_config(cx);
        let application_id = if !enabled && saved.enabled {
            saved.application_id.clone()
        } else {
            self.discord_application_id
                .read(cx)
                .value()
                .trim()
                .to_owned()
        };
        if (enabled || !application_id.is_empty())
            && !crate::discord::valid_application_id(&application_id)
        {
            self.discord_error = Some("Enter the numeric Application ID from Discord's Developer Portal, not a bot token.".into());
            cx.notify();
            return;
        }
        self.discord_error = None;
        cx.emit(SettingsEvent::SetDiscord(crate::discord::Config {
            enabled,
            application_id,
        }));
        cx.notify();
    }

    fn set_lastfm_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let mut config = services::AppServices::lastfm_config(cx).clone();
        if enabled && !config.connected() {
            self.lastfm_status = Some("Connect your Last.fm account first.".into());
            cx.notify();
            return;
        }
        config.enabled = enabled;
        self.lastfm_status = None;
        cx.emit(SettingsEvent::SetLastfm(config));
        cx.notify();
    }

    /// Two steps: ask Last.fm for a sign-in page, then finish once approved.
    fn lastfm_connect(&mut self, cx: &mut Context<Self>) {
        if self.lastfm_busy {
            return;
        }
        let api_key = self.lastfm_api_key.read(cx).value().trim().to_owned();
        let api_secret = self.lastfm_api_secret.read(cx).value().trim().to_owned();
        self.lastfm_busy = true;
        self.lastfm_status = Some("Contacting Last.fm…".into());
        cx.notify();
        if let Some(token) = self.lastfm_token.clone() {
            let reply = crate::lastfm::finish_auth(api_key.clone(), api_secret, token);
            cx.spawn(async move |this, cx| {
                let result = reply.await;
                let _ = this.update(cx, |this, cx| {
                    this.lastfm_busy = false;
                    match result {
                        Ok(Ok(username)) => {
                            this.lastfm_token = None;
                            this.lastfm_status = None;
                            cx.emit(SettingsEvent::SetLastfm(crate::lastfm::Config {
                                enabled: true,
                                api_key,
                                username,
                            }));
                        }
                        Ok(Err(error)) => this.lastfm_status = Some(format!("{error:#}").into()),
                        Err(_) => {
                            this.lastfm_status = Some("Last.fm sign-in did not finish.".into())
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        } else {
            let reply = crate::lastfm::begin_auth(api_key, api_secret);
            cx.spawn(async move |this, cx| {
                let result = reply.await;
                let _ = this.update(cx, |this, cx| {
                    this.lastfm_busy = false;
                    match result {
                        Ok(Ok(start)) => {
                            this.lastfm_token = Some(start.token);
                            this.lastfm_status = Some(
                                "Approve Cadence in your browser, then press Finish connecting."
                                    .into(),
                            );
                            cx.open_url(&start.url);
                        }
                        Ok(Err(error)) => this.lastfm_status = Some(format!("{error:#}").into()),
                        Err(_) => {
                            this.lastfm_status = Some("Last.fm sign-in did not finish.".into())
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn lastfm_disconnect(&mut self, cx: &mut Context<Self>) {
        if self.lastfm_busy {
            return;
        }
        self.lastfm_busy = true;
        let api_key = services::AppServices::lastfm_config(cx).api_key.clone();
        let reply = crate::lastfm::forget_credentials();
        cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.lastfm_busy = false;
                match result {
                    Ok(Ok(())) => {
                        this.lastfm_status = None;
                        cx.emit(SettingsEvent::SetLastfm(crate::lastfm::Config {
                            enabled: false,
                            api_key,
                            username: String::new(),
                        }));
                    }
                    Ok(Err(error)) => this.lastfm_status = Some(format!("{error:#}").into()),
                    Err(_) => {
                        this.lastfm_status = Some("Could not remove the Last.fm sign-in.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn lastfm_section(
        &self,
        palette: CadencePalette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let config = services::AppServices::lastfm_config(cx).clone();
        let connect_label = if self.lastfm_token.is_some() {
            "Finish connecting"
        } else {
            "Connect"
        };
        div().mt_12()
            .child(Self::settings_section_header(palette, "Last.fm", "Optionally scrobble what you listen to."))
            .child(div().mt_4().rounded_2xl().border_1().border_color(rgb(palette.border))
                .bg(rgb(palette.surface_raised)).p_5()
                .child(Self::playback_setting_row(palette, "Scrobble to Last.fm",
                    if config.connected() { "Plays after half the track or four minutes. Retries when offline." } else { "Connect an account below to turn this on." },
                    components::focus_ring_around(palette, &self.lastfm_focus, window, cx,
                        Switch::new("settings-lastfm-enabled").focus_ring(false).checked(config.enabled)
                            .on_click(cx.listener(|this, enabled: &bool, _, cx| this.set_lastfm_enabled(*enabled, cx))))))
                .child(if config.connected() {
                    div().mt_5().pt_5().border_t_1().border_color(rgb(palette.border))
                        .child(div().text_xs().text_color(rgb(palette.text)).child(format!("Connected as {}", config.username)))
                        .child(components::settings_button(palette, "settings-lastfm-disconnect", "Disconnect")
                            .mt_3().on_click(cx.listener(|this, _, _, cx| this.lastfm_disconnect(cx))))
                } else {
                    div().mt_5().pt_5().border_t_1().border_color(rgb(palette.border))
                        .child(div().text_xs().text_color(rgb(palette.text)).child("API key and shared secret"))
                        .child(div().mt_2().child(Input::new(&self.lastfm_api_key).id("settings-lastfm-api-key").w_full()))
                        .child(div().mt_2().child(Input::new(&self.lastfm_api_secret).id("settings-lastfm-api-secret").w_full()))
                        .child(div().mt_2().text_size(tokens::CAPTION_TEXT).text_color(rgb(palette.text_muted))
                            .child("Create a free Last.fm API account, then paste its key and shared secret. The secret stays in your system keychain."))
                        .child(h_flex().mt_3().gap_3()
                            .child(components::settings_button(palette, "settings-lastfm-connect", connect_label)
                                .test_support()
                                .on_click(cx.listener(|this, _, _, cx| this.lastfm_connect(cx))))
                            .child(components::settings_button(palette, "settings-lastfm-portal", "Create API account")
                                .on_click(|_, _, cx| cx.open_url("https://www.last.fm/api/account/create"))))
                })
                .when_some(self.lastfm_status.clone(), |card, status| card.child(div().mt_2().text_sm().text_color(rgb(palette.text)).child(status))))
    }

    fn discord_section(
        &self,
        palette: CadencePalette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = services::AppServices::discord_config(cx).enabled;
        div().mt_12()
            .child(Self::settings_section_header(palette, "Discord", "Optionally show what you're listening to on your Discord profile."))
            .child(div().mt_4().rounded_2xl().border_1().border_color(rgb(palette.border))
                .bg(rgb(palette.surface_raised)).p_5()
                .child(Self::playback_setting_row(palette, "Share listening activity", "Requires Discord desktop. Activity clears while paused or signed out.",
                    components::focus_ring_around(palette, &self.discord_focus, window, cx,
                        Switch::new("settings-discord-enabled").focus_ring(false).checked(enabled)
                            .on_click(cx.listener(|this, enabled: &bool, _, cx| this.save_discord(*enabled, cx))))))
                .child(div().mt_5().pt_5().border_t_1().border_color(rgb(palette.border))
                    .child(div().text_xs().text_color(rgb(palette.text)).child("Discord Application ID"))
                    .child(div().mt_2().flex().gap_3()
                        .child(Input::new(&self.discord_application_id).id("settings-discord-application-id").flex_1().min_w_0())
                        .child(components::settings_button(palette, "settings-discord-save", "Save")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let enabled = services::AppServices::discord_config(cx).enabled;
                                this.save_discord(enabled, cx);
                            }))))
                    .child(div().mt_2().text_size(tokens::CAPTION_TEXT).text_color(rgb(palette.text_muted))
                        .child("Create a Discord application named Cadence and paste its public Application ID. No bot token needed."))
                    .child(components::settings_button(palette, "settings-discord-portal", "Open Discord Developer Portal")
                        .mt_3().on_click(|_, _, cx| cx.open_url("https://discord.com/developers/applications")))
                    .when_some(self.discord_error.clone(), |card, error| card.child(div().mt_2().text_sm().text_color(rgb(palette.text)).child(error)))))
    }

    fn updates_section(
        &self,
        palette: CadencePalette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let check = updater::status(cx).map(|status| status.read(cx).check.clone());
        div()
            .mt_12()
            .child(Self::settings_section_header(
                palette,
                "Updates",
                "New versions install in place from GitHub releases.",
            ))
            .child(
                div()
                    .mt_4()
                    .rounded_2xl()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface_raised))
                    .p_5()
                    .child(Self::version_row(palette, check.as_ref()))
                    .when_some(updater::automatic_checks(cx), |card, enabled| {
                        card.child(
                            div()
                                .mt_5()
                                .pt_5()
                                .border_t_1()
                                .border_color(rgb(palette.border))
                                .child(Self::playback_setting_row(
                                    palette,
                                    "Check for updates automatically",
                                    "Once a day. You choose when to install.",
                                    components::focus_ring_around(
                                        palette,
                                        &self.automatic_updates_focus,
                                        window,
                                        cx,
                                        Switch::new("settings-automatic-updates")
                                            .focus_ring(false)
                                            .checked(enabled)
                                            .on_click(cx.listener(|_, checked: &bool, _, cx| {
                                                updater::set_automatic_checks(*checked, cx);
                                                cx.notify();
                                            })),
                                    ),
                                )),
                        )
                    }),
            )
    }

    /// The running version with the outcome of Sparkle's last check, and a
    /// button that opens Sparkle's window. `None` means this build never
    /// checks, so there is nothing to press.
    fn version_row(palette: CadencePalette, check: Option<&updater::UpdateCheck>) -> Div {
        use updater::UpdateCheck;
        let (detail, emphasized) = match check {
            None => ("This build does not update itself.".to_owned(), false),
            Some(UpdateCheck::Unknown) => ("Not checked yet.".to_owned(), false),
            #[cfg(target_os = "windows")]
            Some(UpdateCheck::Checking) => ("Checking for updates…".to_owned(), false),
            #[cfg(target_os = "windows")]
            Some(UpdateCheck::Downloading) => (
                "Downloading and verifying the update. Cadence will restart when ready.".to_owned(),
                false,
            ),
            Some(UpdateCheck::UpToDate) => ("Up to date.".to_owned(), false),
            Some(UpdateCheck::Available(version)) => {
                (format!("Cadence {version} is ready to install."), true)
            }
            #[cfg(target_os = "windows")]
            Some(UpdateCheck::Failed(error)) => (format!("Update failed: {error}"), false),
            Some(UpdateCheck::Skipped(version)) => (format!("Version {version} skipped."), false),
        };
        #[cfg(target_os = "windows")]
        let busy = matches!(
            check,
            Some(UpdateCheck::Checking | UpdateCheck::Downloading)
        );
        #[cfg(not(target_os = "windows"))]
        let busy = false;
        let action = match check {
            #[cfg(target_os = "windows")]
            Some(UpdateCheck::Checking | UpdateCheck::Downloading) => "Please wait…",
            Some(UpdateCheck::Available(_)) => "Install update",
            _ => "Check for updates",
        };
        h_flex()
            .w_full()
            .justify_between()
            .items_start()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .max_w(relative(0.6))
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(rgb(palette.text))
                            .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(
                        div()
                            .text_size(tokens::BODY_TEXT)
                            .line_height(relative(1.45))
                            .text_color(rgb(if emphasized {
                                palette.text_primary
                            } else {
                                palette.text_muted
                            }))
                            .child(detail),
                    ),
            )
            .when(check.is_some(), |row| {
                row.child(
                    components::settings_button(palette, "settings-check-updates", action)
                        .when(!busy, |button| {
                            button.on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(CheckForUpdates), cx)
                            })
                        })
                        .when(busy, |button| button.opacity(0.6)),
                )
            })
    }

    fn cache_section(&self, palette: CadencePalette, cx: &mut Context<Self>) -> Div {
        let cache = services::AppServices::image_cache(cx);
        let (bytes, limit, reclaimed) = cache.read(cx).usage();
        let detail = if self.cache_cleared {
            "Cache cleared in memory and on disk. Visible artwork reloads as needed.".to_owned()
        } else if reclaimed {
            "Older artwork has been cleared automatically to keep memory bounded.".to_owned()
        } else {
            format!(
                "Artwork uses {:.1} MiB of {} MiB in memory, plus up to {} MiB saved on disk for faster launches. Older images are removed automatically; manual cleanup is optional.",
                bytes as f64 / 1_048_576.,
                limit / 1_048_576,
                disk_cache::DEFAULT_CAP_BYTES / 1_048_576
            )
        };
        div()
            .mt_8()
            .child(Self::settings_section_header(
                palette,
                "Cache",
                "Artwork is managed automatically.",
            ))
            .child(
                div()
                    .mt_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(palette.text_muted))
                            .child(detail),
                    )
                    .child(
                        components::settings_button(palette, "settings-clear-cache", "Clear cache")
                            .test_support()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cache.update(cx, |cache, cx| cache.clear(window, cx));
                                http::clear_disk_cache();
                                this.cache_cleared = true;
                                cx.refresh_windows();
                            })),
                    ),
            )
    }

    fn settings_section_header(
        palette: CadencePalette,
        title: &'static str,
        detail: &'static str,
    ) -> Div {
        div()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text_primary))
                    .child(title),
            )
            .child(
                div()
                    .mt_1()
                    .text_size(tokens::BODY_TEXT)
                    .text_color(rgb(palette.text_muted))
                    .child(detail),
            )
    }

    fn playback_setting_row(
        palette: CadencePalette,
        title: &'static str,
        description: &'static str,
        field: impl IntoElement,
    ) -> Div {
        h_flex()
            .w_full()
            .justify_between()
            .items_start()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .max_w(relative(0.6))
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(rgb(palette.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(tokens::BODY_TEXT)
                            .line_height(relative(1.45))
                            .text_color(rgb(palette.text_muted))
                            .child(description),
                    ),
            )
            .child(div().child(field))
    }

    fn appearance_option(
        &self,
        id: &'static str,
        icon: CadenceIcon,
        label: &'static str,
        preference: ThemePreference,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = appearance::Appearance::palette(cx);
        let selected = appearance::Appearance::preference(cx) == preference;
        components::filled_button(palette, id)
            .h_11()
            .flex_1()
            .gap_2()
            .rounded(tokens::CONTROL_RADIUS)
            .bg(if selected {
                rgb(palette.selection)
            } else {
                rgb(palette.control)
            })
            .text_sm()
            .font_weight(if selected {
                gpui_kit::FontWeight::SEMIBOLD
            } else {
                gpui_kit::FontWeight::MEDIUM
            })
            .text_color(rgb(palette.text_primary))
            .hover(|style| style.bg(rgb(palette.control_hover)))
            .child(components::icon(
                icon,
                tokens::MENU_ICON,
                palette.text_primary,
            ))
            .child(label)
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(SettingsEvent::SetTheme(preference));
            }))
    }
}

fn mascot_index(mascot: MascotPreference) -> usize {
    match mascot {
        MascotPreference::None => 0,
        MascotPreference::RomeoVespa => 1,
        MascotPreference::VespaDuo => 2,
        MascotPreference::TarantellaDancer => 3,
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.page(window, cx)
    }
}
