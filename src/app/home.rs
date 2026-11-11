//! The Home page: the listener's most played artists and tracks.
//!
//! Two small Spotify requests, made only when the page is opened and its data
//! is more than half an hour old. Nothing runs in the background.
use super::page::PageEvent;
use super::*;
use gpui_kit::TestSupportExt as _;
use std::time::SystemTime;

/// Top items drift over weeks, so there is no reason to fetch them often.
const HOME_STALE_TIME: Duration = Duration::from_secs(30 * 60);

pub(super) struct HomePage {
    backend: BackendHandle,
    session: Entity<session::Session>,
    track_list: Entity<track_list::TrackList>,
    artists: Arc<[model::Artist]>,
    tracks: Arc<[model::Track]>,
    loading: bool,
    error: Option<SharedString>,
    /// The saved sign-in lacks the permission to read top items.
    permission_missing: bool,
    loaded_at: Option<SystemTime>,
    visible: bool,
    request: Option<gpui_kit::Task<()>>,
    scroll: gpui_kit::ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PageEvent> for HomePage {}

impl HomePage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| track_list::TrackList::new(cx).with_inline_rows());
        let session = services::AppServices::session(cx);
        let subscriptions = vec![
            page::forward(&track_list, cx),
            cx.subscribe(&session, |this, _, event: &session::SessionEvent, cx| {
                this.handle_session_event(event, cx)
            }),
        ];
        Self {
            backend,
            session,
            track_list,
            artists: Arc::default(),
            tracks: Arc::default(),
            loading: false,
            error: None,
            permission_missing: false,
            loaded_at: None,
            visible: false,
            request: None,
            scroll: gpui_kit::ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn handle_session_event(&mut self, event: &session::SessionEvent, cx: &mut Context<Self>) {
        match event {
            // A different account, or a fresh sign-in that may now carry the
            // permission: start over.
            session::SessionEvent::LoggedOut | session::SessionEvent::Restarted => self.reset(cx),
            session::SessionEvent::Ready => {
                self.reset(cx);
                if self.visible {
                    self.load(cx);
                }
            }
            _ => {}
        }
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.loading = false;
        self.artists = Arc::default();
        self.tracks = Arc::default();
        self.error = None;
        self.permission_missing = false;
        self.loaded_at = None;
        self.track_list.update(cx, |list, cx| list.clear(cx));
        cx.notify();
    }

    /// Called by the workspace as the route changes, so the page only fetches
    /// while it is the one on screen.
    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if !visible {
            self.track_list.update(cx, |list, cx| list.close_menu(cx));
            return;
        }
        let fresh = self
            .loaded_at
            .is_some_and(|at| at.elapsed().is_ok_and(|elapsed| elapsed < HOME_STALE_TIME));
        if !fresh && !self.loading && !self.permission_missing {
            self.load(cx);
        }
    }

    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        let reply = catalog::request(&self.backend, |respond| BackendCommand::LoadHome {
            respond,
        });
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                page.request = None;
                page.loading = false;
                match result {
                    Ok(mut home) => {
                        home.artists.truncate(model::HOME_ARTIST_LIMIT);
                        home.tracks.truncate(model::HOME_TRACK_LIMIT);
                        page.artists = home.artists.into();
                        page.tracks = home.tracks.into();
                        page.loaded_at = Some(SystemTime::now());
                        page.error = None;
                        cx.emit(PageEvent::Loaded);
                    }
                    Err(error) => {
                        page.permission_missing =
                            error.as_str() == model::HomePermissionMissing.to_string();
                        if !page.permission_missing {
                            page.error = Some(error.clone());
                            cx.emit(PageEvent::Failed(error));
                        }
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn artist_card(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = appearance::Appearance::palette(cx);
        let artist = &self.artists[index];
        let cache = services::AppServices::image_cache(cx);
        components::button(palette, ("home-artist", index))
            .test_support()
            .w(HOME_CARD_WIDTH + Rems(0.75))
            .min_w_0()
            .p_1p5()
            .flex_col()
            .items_start()
            .justify_start()
            .gap_2()
            .rounded_xl()
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(components::rectangular_artwork(
                palette,
                &cache,
                artist.artwork_url.as_deref(),
                tokens::SEARCH_ARTIST_ARTWORK,
                HOME_CARD_WIDTH,
                CadenceIcon::Person,
            ))
            .child(
                div()
                    .w_full()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(artist.name.clone()),
            )
            .child(components::page_detail(palette, "Artist"))
            // Capture just the index, resolving the current data on activation.
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(artist) = this.artists.get(index) {
                    cx.emit(PageEvent::OpenArtist(model::ArtistRef {
                        name: artist.name.clone(),
                        source_id: Some(artist.source_id.clone()),
                        spotify_uri: artist.spotify_uri.clone(),
                    }));
                }
            }))
            .into_any_element()
    }

    fn permission_prompt(&self, palette: CadencePalette, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_3()
            .p_6()
            .rounded(tokens::PANEL_RADIUS)
            .border_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text_primary))
                    .child("One more permission needed"),
            )
            .child(components::page_detail(
                palette,
                "Home reads your top artists and tracks. Sign in again to allow it. Your settings and playlists stay.",
            ))
            .child(
                components::pill(palette, "home-sign-in-again", "Sign in again", true)
                    .test_support()
                    .h_10()
                    .mt_1()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.session.update(cx, |session, cx| session.logout(cx));
                    })),
            )
    }
}

/// Wide enough for five across at the default window width.
const HOME_CARD_WIDTH: Rems = Rems(9.5);

impl Render for HomePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let heading = |id, text| {
            div()
                .id(id)
                .test_support()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(rgb(palette.text_primary))
                .child(text)
        };
        let loaded = self.loaded_at.is_some();
        let content = if self.permission_missing {
            self.permission_prompt(palette, cx)
        } else if self.error.is_some() && !loaded {
            div().child(components::empty_state(
                palette,
                "Unable to load your top artists and tracks",
            ))
        } else if !loaded {
            div().child(components::list_skeleton_rows(palette, 4))
        } else if self.artists.is_empty() && self.tracks.is_empty() {
            div().child(components::empty_state(
                palette,
                "Listen to more music and your top artists and tracks will show up here",
            ))
        } else {
            let mut artists = div().flex().flex_wrap().gap_4();
            for index in 0..self.artists.len() {
                artists = artists.child(self.artist_card(index, cx));
            }
            self.track_list.update(cx, |list, cx| {
                list.show("home-tracks", self.tracks.clone(), cx);
            });
            div()
                .flex()
                .flex_col()
                .gap_6()
                .when(!self.artists.is_empty(), |content| {
                    content.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(heading("home-artists-heading", "Your top artists"))
                            .child(artists),
                    )
                })
                .when(!self.tracks.is_empty(), |content| {
                    content.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(heading("home-tracks-heading", "Your top tracks"))
                            .child(self.track_list.clone()),
                    )
                })
        };
        div()
            .id("home-page")
            .size_full()
            .min_h_0()
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.text))
            .child(
                div()
                    .id("home-scroll")
                    .test_support()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .p_8()
                            .pt_3()
                            .pr(px(48.))
                            .w_full()
                            .child(components::page_heading(
                                palette,
                                "Home",
                                "Based on what you play most",
                            ))
                            .child(div().mt_4().child(content)),
                    ),
            )
    }
}
