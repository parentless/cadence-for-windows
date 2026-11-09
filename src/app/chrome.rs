use super::*;
use gpui_kit::TestSupportExt as _;

/// What the toolbar asks the workspace to do.
pub(super) enum ToolbarEvent {
    QueryChanged(String),
    SubmitSearch,
    Navigate(Route),
    OpenSettings,
    Connect,
    Logout,
    /// The account menu opened, so anything else overlaying the page should go.
    MenuOpened,
}

/// The bar above the page: search on the left, account on the right.
pub(super) struct Toolbar {
    search_input: Entity<InputState>,
    suggestions: Vec<cached_search::Match>,
    suggestion_query: String,
    suggestion_selection: Option<usize>,
    suggestion_task: Option<gpui_kit::Task<()>>,
    search_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    library: Entity<library::Library>,
    pub(super) artist_header: Option<Entity<artist_header::ArtistHeader>>,
    menu_open: bool,
    session: Entity<session::Session>,
    player: Entity<player::Player>,
    route: Route,
    /// Where the back button goes, when the current route has one.
    back_target: Option<Route>,
    /// The workspace's standing failure, shown under the account name.
    error: Option<SharedString>,
    _search_subscription: Subscription,
    _update_subscription: Option<Subscription>,
    update_version: Option<String>,
    update_generation: usize,
}

impl EventEmitter<ToolbarEvent> for Toolbar {}

impl Toolbar {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search Spotify"));
        let search_subscription = cx.subscribe_in(
            &search_input,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    this.queue_suggestions(&query, cx);
                    cx.emit(ToolbarEvent::QueryChanged(query));
                }
                InputEvent::PressEnter { .. } => {
                    if let Some(index) = this.suggestion_selection {
                        this.play_suggestion(index, cx);
                    } else {
                        this.dismiss_suggestions(cx);
                        cx.emit(ToolbarEvent::SubmitSearch);
                    }
                }
                _ => {}
            },
        );
        let update_subscription = updater::status(cx).map(|status| {
            cx.observe(&status, |this, status, cx| {
                let version = match &status.read(cx).check {
                    updater::UpdateCheck::Available(version) => Some(version.clone()),
                    _ => None,
                };
                if this.update_version != version {
                    this.update_version = version;
                    this.update_generation = this.update_generation.wrapping_add(1);
                    cx.notify();
                }
            })
        });
        let update_version = updater::status(cx).and_then(|status| match &status.read(cx).check {
            updater::UpdateCheck::Available(version) => Some(version.clone()),
            _ => None,
        });
        Self {
            _update_subscription: update_subscription,
            update_version,
            update_generation: 0,
            search_input,
            suggestions: Vec::new(),
            suggestion_query: String::new(),
            suggestion_selection: None,
            suggestion_task: None,
            search_bounds: Rc::default(),
            library: services::AppServices::library(cx),
            artist_header: None,
            menu_open: false,
            session: services::AppServices::session(cx),
            player: services::AppServices::player(cx),
            route: Route::LikedSongs,
            back_target: None,
            error: None,
            _search_subscription: search_subscription,
        }
    }

    #[cfg(test)]
    pub(super) fn seed_update_available(&mut self, cx: &mut Context<Self>) {
        self.update_version = Some("9.9.9".into());
        self.update_generation += 1;
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn seed_cached_suggestions(&mut self, cx: &mut Context<Self>) {
        self.dismiss_suggestions(cx);
        let library = self.library.read(cx);
        self.suggestions = cached_search::find(
            "Song",
            &[
                library.favorites().clone(),
                library.recently_played().clone(),
                library.liked_tracks().clone(),
            ],
        );
        self.suggestion_selection = Some(1);
        cx.notify();
    }

    /// Pushes the workspace state the toolbar reflects but does not own.
    pub(super) fn show(
        &mut self,
        route: Route,
        back_target: Option<Route>,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.route != route || self.back_target != back_target || self.error != error {
            if self.route != route {
                self.dismiss_suggestions(cx);
            }
            self.route = route;
            self.back_target = back_target;
            self.error = error;
            cx.notify();
        }
    }

    pub(super) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search_input.read(cx).focus_handle(cx), cx);
    }

    pub(super) fn clear_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_open {
            self.menu_open = false;
            cx.notify();
        }
    }

    pub(super) fn close_suggestions(&mut self, cx: &mut Context<Self>) -> bool {
        let open = !self.suggestions.is_empty() || self.suggestion_task.is_some();
        self.dismiss_suggestions(cx);
        open
    }

    fn dismiss_suggestions(&mut self, cx: &mut Context<Self>) {
        self.suggestion_task = None;
        self.suggestion_selection = None;
        if !self.suggestions.is_empty() {
            self.suggestions.clear();
            cx.notify();
        }
    }

    fn queue_suggestions(&mut self, query: &str, cx: &mut Context<Self>) {
        self.dismiss_suggestions(cx);
        self.suggestion_query = query.trim().chars().take(96).collect();
        if self.suggestion_query.chars().count() < 2 {
            return;
        }
        let query = self.suggestion_query.clone();
        self.suggestion_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let Ok(sources) = this.update(cx, |toolbar, cx| {
                let library = toolbar.library.read(cx);
                [
                    library.favorites().clone(),
                    library.recently_played().clone(),
                    library.liked_tracks().clone(),
                ]
            }) else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { cached_search::find(&query, &sources) })
                .await;
            let _ = this.update(cx, |toolbar, cx| {
                toolbar.suggestion_task = None;
                toolbar.suggestions = result;
                cx.notify();
            });
        }));
    }

    fn play_suggestion(&mut self, index: usize, cx: &mut Context<Self>) {
        let found = self.suggestions.get(index).cloned();
        self.play_cached(found.as_ref(), cx);
    }

    fn play_cached(&mut self, found: Option<&cached_search::Match>, cx: &mut Context<Self>) {
        let track = found
            .and_then(|found| found.resolve(self.library.read(cx)))
            .cloned();
        self.dismiss_suggestions(cx);
        if let Some(track) = track {
            self.player.update(cx, |player, cx| {
                player.play_context(vec![track], 0, cx);
            });
        }
    }

    fn suggestion_menu(
        &self,
        palette: CadencePalette,
        width: Rems,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let library = self.library.read(cx);
        let mut menu = components::menu_surface(palette)
            .id("cached-search-suggestions")
            .test_support()
            .absolute()
            .top_12()
            .left_0()
            .w(width)
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(palette.text_muted))
                    .child("From your library"),
            );
        for (index, found) in self.suggestions.iter().enumerate() {
            let Some(track) = found.resolve(library) else {
                continue;
            };
            let found = found.clone();
            menu = menu.child(
                components::button(palette, ("cached-search-track", index))
                    .test_support()
                    .w_full()
                    .h_12()
                    .px_3()
                    .gap_2()
                    .rounded_lg()
                    .justify_start()
                    .when(self.suggestion_selection == Some(index), |row| {
                        row.bg(rgb(palette.selection))
                    })
                    .hover(|style| style.bg(rgb(palette.surface_hover)))
                    .child(components::icon(
                        CadenceIcon::MusicNote,
                        tokens::CONTROL_ICON,
                        palette.text_muted,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(rgb(palette.text_primary))
                                    .child(track.title.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(palette.text_muted))
                                    .child(track.artist.clone()),
                            ),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.play_cached(Some(&found), cx)),
                    ),
            );
        }
        menu.on_mouse_down_out(
            cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                if !this
                    .search_bounds
                    .get()
                    .is_some_and(|bounds| bounds.contains(&event.position))
                {
                    this.dismiss_suggestions(cx);
                }
            }),
        )
    }

    fn search_field(
        &mut self,
        palette: CadencePalette,
        compact: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let width = if self.route == Route::Artist {
            Rems(if compact { 13. } else { 23. })
        } else if compact {
            tokens::COMPACT_SEARCH_FIELD_WIDTH
        } else {
            tokens::SEARCH_FIELD_WIDTH
        };
        components::glass_search_frame(palette, &self.search_input, window, cx)
            .id("search-field")
            .test_support()
            .relative()
            .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if this.suggestions.is_empty() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "down" => {
                        this.suggestion_selection = Some(
                            this.suggestion_selection
                                .map_or(0, |i| (i + 1) % this.suggestions.len()),
                        );
                    }
                    "up" => {
                        this.suggestion_selection = Some(
                            this.suggestion_selection
                                .map_or(this.suggestions.len() - 1, |i| {
                                    (i + this.suggestions.len() - 1) % this.suggestions.len()
                                }),
                        );
                    }
                    "escape" => this.dismiss_suggestions(cx),
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .w(width)
            .min_w_0()
            .h_10()
            .justify_start()
            .gap_2p5()
            .text_sm()
            .text_color(rgb(palette.text))
            .child(components::icon(
                CadenceIcon::Search,
                tokens::FIELD_ICON,
                palette.text_primary,
            ))
            .child(
                Input::new(&self.search_input)
                    .id("search-input")
                    .appearance(false)
                    .text_color(rgb(palette.text_primary))
                    .bordered(false)
                    .focus_bordered(false)
                    .flex_1()
                    .min_w_0()
                    .h_full(),
            )
            .child(
                div()
                    .when(self.route == Route::Artist && compact, |hint| hint.hidden())
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .bg(rgb(palette.surface_raised))
                    .bg(gpui_kit::rgba(0xFFFFFF14))
                    .text_color(rgb(palette.text))
                    .text_size(tokens::CAPTION_TEXT)
                    .child(if cfg!(target_os = "macos") {
                        "⌘ K"
                    } else {
                        "Ctrl K"
                    }),
            )
            .child(
                gpui_kit::canvas(
                    {
                        let bounds = self.search_bounds.clone();
                        move |rect, _, _| bounds.set(Some(rect))
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when(!self.suggestions.is_empty(), |field| {
                field.child(deferred(self.suggestion_menu(palette, width, cx)))
            })
    }

    /// The line under the account name: the newest failure if there is one,
    /// otherwise how far the Spotify connection has got.
    fn account_detail(&self, cx: &App) -> SharedString {
        if let Some(error) = self.player.read(cx).error() {
            return error.clone();
        }
        if let Some(error) = &self.error {
            return error.clone();
        }
        match self.session.read(cx).state() {
            ConnectionState::Starting => "Starting Spotify…".into(),
            ConnectionState::Failed => "Backend unavailable".into(),
            ConnectionState::SetupRequired => "Developer app required".into(),
            ConnectionState::AuthorizationRequired => "Not connected".into(),
            ConnectionState::Connecting => "Connecting…".into(),
            ConnectionState::Ready => "Spotify connected".into(),
        }
    }

    fn profile_name(&self, cx: &App) -> SharedString {
        self.session.read(cx).profile().map_or_else(
            || SharedString::new_static("Spotify account"),
            |profile| profile.display_name.clone(),
        )
    }

    fn account_menu(&self, palette: CadencePalette, cx: &mut Context<Self>) -> impl IntoElement {
        let profile_name = self.profile_name(cx);
        let can_connect = matches!(
            self.session.read(cx).state(),
            ConnectionState::AuthorizationRequired
        );

        components::solid_menu_surface(palette)
            .on_mouse_up_out(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_menu(cx)),
            )
            .absolute()
            .top_12()
            .right_0()
            .child(
                div()
                    .px_2p5()
                    .py_2()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text_primary))
                            .child(profile_name),
                    )
                    .child(
                        div()
                            .mt_0p5()
                            .text_xs()
                            .text_color(rgb(palette.text_muted))
                            .child(self.account_detail(cx)),
                    ),
            )
            .when(can_connect, |menu| {
                menu.child(
                    components::menu_item(
                        palette,
                        "account-connect",
                        CadenceIcon::Key,
                        "Log in with Spotify",
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu_open = false;
                        cx.emit(ToolbarEvent::Connect);
                        cx.notify();
                    })),
                )
            })
            .child(
                div()
                    .mt_1()
                    .pt_1p5()
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .child(
                        components::menu_item(
                            palette,
                            "account-settings",
                            CadenceIcon::Settings,
                            "Settings",
                            false,
                        )
                        .test_support()
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(ToolbarEvent::OpenSettings);
                        })),
                    ),
            )
            .when(self.session.read(cx).is_ready(), |menu| {
                menu.child(
                    components::menu_item(
                        palette,
                        "account-logout",
                        CadenceIcon::SignOut,
                        "Logout",
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu_open = false;
                        cx.emit(ToolbarEvent::Logout);
                        cx.notify();
                    })),
                )
            })
    }
}

impl Render for Toolbar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let compact = is_compact_content_layout(window);
        let profile_name = self.profile_name(cx);
        let profile = self.session.read(cx).profile().cloned();
        let profile_artwork = profile
            .as_ref()
            .and_then(|profile| profile.artwork_url.as_deref());
        let showing_settings = self.route == Route::Settings;
        let artist_dock_width = Rems(if compact { 7.5 } else { 10.5 });

        div()
            .h(tokens::TOOLBAR_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_7()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .when(self.route == Route::Artist, |group| {
                        group.flex_1().min_w_0()
                    })
                    .when_some(self.back_target, |group, origin| {
                        group.child(
                            components::icon_button(
                                palette,
                                "detail-back",
                                CadenceIcon::ChevronLeft,
                            )
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(ToolbarEvent::Navigate(origin));
                                },
                            )),
                        )
                    })
                    .when(showing_settings, |group| {
                        group.child(
                            div()
                                .h_10()
                                .flex()
                                .items_center()
                                .text_lg()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(rgb(palette.text_primary))
                                .child("Settings"),
                        )
                    })
                    .when(!showing_settings, |group| {
                        group.child(self.search_field(palette, compact, window, cx))
                    }),
            )
            .child(
                div()
                    .relative()
                    .when(self.route == Route::Artist, |anchor| {
                        anchor.ml(artist_dock_width + Rems(3.25)).flex_none()
                    })
                    .when(self.route == Route::Artist, |anchor| {
                        anchor.child(
                            div()
                                .absolute()
                                .right(Rems(3.25))
                                .top(Rems(0.25))
                                .w(artist_dock_width)
                                .h(Rems(2.))
                                .child(
                                    gpui_kit::canvas(
                                        {
                                            let header = self.artist_header.clone();
                                            move |bounds, _, cx| {
                                                if let Some(header) = &header
                                                    && header.read(cx).target_bounds != Some(bounds)
                                                {
                                                    let header = header.clone();
                                                    cx.defer(move |cx| {
                                                        header.update(cx, |header, cx| {
                                                            if header.target_bounds != Some(bounds)
                                                            {
                                                                header.target_bounds = Some(bounds);
                                                                cx.notify();
                                                            }
                                                        });
                                                    });
                                                }
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .size_full(),
                                ),
                        )
                    })
                    .child(
                        components::filled_button(palette, "account")
                            .hover(|style| style.opacity(0.9))
                            .test_support()
                            .size_10()
                            .rounded_full()
                            .text_color(rgb(palette.on_accent))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(components::profile_avatar(
                                profile_artwork,
                                components::initials(&profile_name),
                                window,
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu_open = !this.menu_open;
                                if this.menu_open {
                                    cx.emit(ToolbarEvent::MenuOpened);
                                }
                                cx.notify();
                            })),
                    )
                    .when(self.update_version.is_some(), |anchor| {
                        anchor.child(
                            components::bare_button("update-available")
                                .test_support()
                                .absolute()
                                .right_0()
                                .top_0()
                                .size_3()
                                .rounded_full()
                                .bg(rgb(0xF04452))
                                .aria_label("Update available — open Settings")
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Update available")
                                        .build(window, cx)
                                })
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(ToolbarEvent::OpenSettings)),
                                )
                                .with_animation(
                                    ("update-arrived", self.update_generation),
                                    Animation::new(Duration::from_millis(650)),
                                    |badge, progress| {
                                        // One short jump on arrival, then a static dot. No repeating timer.
                                        let jump =
                                            (progress * std::f32::consts::PI * 2.).sin().abs()
                                                * (1. - progress)
                                                * 6.;
                                        badge.top(px(-jump))
                                    },
                                ),
                        )
                    })
                    .when(self.menu_open, |anchor| {
                        anchor.child(deferred(self.account_menu(palette, cx)))
                    }),
            )
    }
}

/// Asks before throwing away the saved Spotify developer app, which cannot be
/// undone from inside Cadence.
pub(super) fn spotify_app_change_confirmation(
    palette: CadencePalette,
    signed_in: bool,
    cancel: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
    confirm: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let consequence = if signed_in {
        "This signs you out, removes the saved Client ID, and restarts Spotify setup. Your Cadence favorites and settings stay."
    } else {
        "This removes the saved Client ID and restarts Spotify setup. Your Cadence favorites and settings stay."
    };
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .left_0()
        .occlude()
        .bg(palette.scrim)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(tokens::DIALOG_WIDTH)
                .p_6()
                .rounded_2xl()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(rgb(palette.text_primary))
                        .child("Change Spotify developer app?"),
                )
                .child(
                    div()
                        .mt_2p5()
                        .text_sm()
                        .line_height(relative(1.5))
                        .text_color(rgb(palette.text))
                        .child(consequence),
                )
                .child(
                    div()
                        .mt_6()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            components::settings_button(
                                palette,
                                "cancel-spotify-app-change",
                                "Cancel",
                            )
                            .test_support()
                            .on_click(cancel),
                        )
                        .child(
                            components::filled_button(palette, "confirm-spotify-app-change")
                                .h_10()
                                .px_3p5()
                                .rounded(tokens::CONTROL_RADIUS)
                                .bg(rgb(palette.destructive))
                                .text_size(tokens::BODY_TEXT)
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(rgb(palette.on_destructive))
                                .hover(|style| style.opacity(0.88))
                                .child("Change developer app")
                                .on_click(confirm),
                        ),
                ),
        )
}
