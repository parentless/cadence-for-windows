use super::{BackendProbe, initialize, press_key, settle, tab_to, track, workspace};
use crate::app::{
    COLLAPSED_SIDEBAR_WIDTH, EXPANDED_SIDEBAR_WIDTH, Route, Workspace, appearance, assets,
    components, onboarding, services, windows,
};
use crate::backend::{BackendCommand, BackendEvent};
use crate::model;
use crate::storage::{MascotPreference, ThemePreference};
use gpui_kit::component::Root;
use gpui_kit::test::{ElementSnapshot, TestWindowExt};
use gpui_kit::{
    AnyView, App, AppContext, HeadlessAppContext, InputEvent as _, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, NoopTextSystem, Pixels, Point, Size, WeakEntity, Window,
    WindowHandle, point, px, size,
};
use std::sync::Arc;
use std::time::Duration;

struct Fixture {
    cx: HeadlessAppContext,
    window: WindowHandle<Root>,
    workspace: Option<WeakEntity<Workspace>>,
    backend: BackendProbe,
}

impl Fixture {
    fn at_width(width: f32) -> Self {
        // Set initial bounds before child views measure responsive columns.
        let mut cx = HeadlessAppContext::with_asset_source(
            Arc::new(NoopTextSystem),
            Arc::new(assets::CadenceAssets),
        );
        let backend = cx.update(|cx| initialize(cx, ThemePreference::Dark));
        let (window, workspace) = workspace(&mut cx, width, 820.);
        settle(&mut cx, window.into());
        Self {
            cx,
            window,
            workspace: Some(workspace.downgrade()),
            backend,
        }
    }

    fn new(settings: bool) -> Self {
        let mut cx = HeadlessAppContext::with_asset_source(
            Arc::new(NoopTextSystem),
            Arc::new(assets::CadenceAssets),
        );
        let backend = cx.update(|cx| initialize(cx, ThemePreference::Dark));
        let (window, workspace) = workspace(&mut cx, 1280., 820.);
        if settings {
            cx.update(|cx| workspace.update(cx, |workspace, cx| workspace.open_settings(cx)));
        }
        settle(&mut cx, window.into());
        Self {
            cx,
            window,
            workspace: Some(workspace.downgrade()),
            backend,
        }
    }

    fn setup() -> Self {
        Self::setup_required(
            ThemePreference::Light,
            size(px(1280.), px(820.)),
            |window, cx| {
                appearance::Appearance::attach(window, cx);
                cx.new(|cx| onboarding::Onboarding::new(window, cx)).into()
            },
        )
    }

    /// The sign-in window as the app opens it, over a setup that still needs
    /// a Client ID.
    fn sign_in_window() -> Self {
        Self::setup_required(
            ThemePreference::Light,
            size(px(1140.), px(720.)),
            |window, cx| {
                cx.new(|cx| windows::OnboardingWindow::new(window, cx))
                    .into()
            },
        )
    }

    /// A window whose root is `root_view`, over a setup that still needs a
    /// Client ID.
    fn setup_required(
        theme: ThemePreference,
        window_size: Size<Pixels>,
        root_view: impl FnOnce(&mut Window, &mut App) -> AnyView,
    ) -> Self {
        let mut cx = HeadlessAppContext::with_asset_source(
            Arc::new(NoopTextSystem),
            Arc::new(assets::CadenceAssets),
        );
        let backend = cx.update(|cx| {
            let backend = initialize(cx, theme);
            services::AppServices::session(cx).update(cx, |session, cx| {
                session.handle_backend_event(BackendEvent::SetupRequired, cx);
            });
            backend
        });
        let window = cx
            .open_window(window_size, |window, cx| {
                let view = root_view(window, cx);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("setup window");
        settle(&mut cx, window.into());
        Self {
            cx,
            window,
            workspace: None,
            backend,
        }
    }

    fn app_change_confirmation_open(&mut self) -> bool {
        self.update(|_, cx| {
            services::AppServices::session(cx)
                .read(cx)
                .app_change_confirmation_open()
        })
    }

    fn route(&mut self) -> Route {
        let workspace = self.workspace.clone().expect("workspace fixture");
        self.update(|_, cx| {
            workspace
                .upgrade()
                .expect("live workspace")
                .read(cx)
                .router
                .route()
        })
    }

    fn play(&mut self, current: model::Track) {
        self.player_event(BackendEvent::PlaybackSnapshotLoaded {
            current,
            next: vec![track(1)],
            position_ms: 0,
        });
    }

    fn player_event(&mut self, event: BackendEvent) {
        self.update(|_, cx| {
            services::AppServices::player(cx).update(cx, |player, cx| {
                player.handle_backend_event(event, cx);
            });
        });
    }

    fn queued_source_ids(&mut self) -> Vec<String> {
        self.update(|_, cx| {
            services::AppServices::player(cx)
                .read(cx)
                .queue()
                .iter()
                .map(|track| track.source_id.clone())
                .collect()
        })
    }

    fn marked_played(&mut self) -> String {
        match self.backend.commands.try_recv().expect("history request") {
            BackendCommand::MarkPlayed(track) => track.source_id,
            other => panic!("expected a history request, got {other:?}"),
        }
    }

    fn requested_artist(&mut self) -> String {
        match self.backend.commands.try_recv().expect("artist request") {
            BackendCommand::LoadArtist { source_id, .. } => source_id,
            other => panic!("expected an artist request, got {other:?}"),
        }
    }

    fn requested_album(&mut self) -> String {
        match self.backend.commands.try_recv().expect("album request") {
            BackendCommand::LoadAlbum { source_id, .. } => source_id,
            other => panic!("expected an album request, got {other:?}"),
        }
    }

    fn update<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let result = self
            .cx
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
            .expect("fixture window");
        settle(&mut self.cx, self.window.into());
        result
    }

    fn wait_for(&mut self, predicate: impl Fn(&mut Window, &mut App) -> bool) {
        for _ in 0..20 {
            if self.update(&predicate) {
                return;
            }
            self.cx.advance_clock(Duration::from_millis(10));
        }
        panic!("UI did not settle within 200 ms");
    }

    fn no_commands(&mut self) {
        assert!(matches!(
            self.backend.commands.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    /// The pointer half of a scrub, split so a test can act between the press
    /// and the release. `TestWindowExt::drag` only does the whole gesture.
    fn mouse_down(&mut self, position: Point<gpui_kit::Pixels>) {
        self.update(|window, cx| {
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
        });
    }

    fn right_click_track(&mut self, index: usize) {
        self.right_click_track_at(index, 150.);
    }

    fn right_click_track_at(&mut self, index: usize, offset: f32) {
        let position = self.update(|window, _| {
            let bounds = window.find(("spotify-track", index)).bounds();
            point(bounds.origin.x + px(offset), bounds.center().y)
        });
        self.update(|window, cx| {
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Right,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
        });
        self.update(|window, cx| {
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Right,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
        });
    }

    fn mouse_move_to(&mut self, position: Point<gpui_kit::Pixels>) {
        self.update(|window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
        });
    }

    fn mouse_up(&mut self, position: Point<gpui_kit::Pixels>) {
        self.update(|window, cx| {
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
        });
    }

    fn press(&mut self, key: &str) {
        press_key(&mut self.cx, self.window.into(), key);
    }

    fn focused(&mut self, id: &'static str) -> bool {
        self.update(|window, _| window.find(id).focused() == Some(true))
    }

    fn tab_to(&mut self, id: impl Into<gpui_kit::ElementId>) {
        tab_to(&mut self.cx, self.window.into(), id);
    }
}

fn cached_search_fixture() -> Fixture {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: (0..30).map(track).collect(),
                    playlists: vec![],
                },
                0,
                cx,
            );
        });
        window.click("search-input", cx);
    });
    fixture
}

#[test]
fn cached_search_debounces_without_network_and_supports_keyboard_playback() {
    let mut fixture = cached_search_fixture();
    fixture.update(|window, cx| window.input("Song", cx));
    fixture.update(|window, _| assert!(window.try_find("cached-search-suggestions").is_none()));
    fixture.cx.advance_clock(Duration::from_millis(130));
    fixture.wait_for(|window, _| window.try_find("cached-search-suggestions").is_some());
    fixture.no_commands();
    fixture.update(|window, _| {
        for i in 0usize..6 {
            assert!(window.try_find(("cached-search-track", i)).is_some());
        }
        assert!(window.try_find(("cached-search-track", 6usize)).is_none());
    });
    fixture.update(|window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    let BackendCommand::PlayContext { tracks, index } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected cached track playback")
    };
    assert_eq!(tracks.len(), 1);
    assert_eq!(index, 0);
    assert_eq!(tracks[0].source_id, track(0).source_id);
    fixture.update(|window, cx| {
        assert!(window.try_find("cached-search-suggestions").is_none());
        assert!(services::AppServices::player(cx).read(cx).loading());
    });
    fixture.no_commands();
}

#[test]
fn cached_suggestions_click_and_escape_dismissal_work() {
    let mut fixture = cached_search_fixture();
    fixture.update(|window, cx| window.input("Song", cx));
    fixture.cx.advance_clock(Duration::from_millis(130));
    fixture.wait_for(|window, _| window.try_find("cached-search-suggestions").is_some());
    fixture.update(|window, cx| window.click(("cached-search-track", 1usize), cx));
    let BackendCommand::PlayContext { tracks, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected cached playback")
    };
    assert_eq!(tracks[0].source_id, track(1).source_id);
    fixture.update(|window, cx| {
        window.click("search-input", cx);
        window.input(" 2", cx);
    });
    fixture.cx.advance_clock(Duration::from_millis(130));
    fixture.wait_for(|window, _| window.try_find("cached-search-suggestions").is_some());
    fixture.update(|window, cx| window.press("escape", cx));
    fixture.update(|window, _| assert!(window.try_find("cached-search-suggestions").is_none()));
    fixture.no_commands();
}

#[test]
fn cached_suggestions_reject_replaced_library_rows_and_close_outside() {
    let mut fixture = cached_search_fixture();
    fixture.update(|window, cx| window.input("Song", cx));
    fixture.cx.advance_clock(Duration::from_millis(130));
    fixture.wait_for(|window, _| window.try_find("cached-search-suggestions").is_some());
    fixture.update(|window, cx| {
        let position = window
            .find(("cached-search-track", 0usize))
            .bounds()
            .center();
        window.dispatch_event(
            MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.clear(cx);
            library.handle_backend_event(
                BackendEvent::LocalStateLoaded {
                    favorites: vec![],
                    pinned_playlists: vec![],
                    recently_played: vec![],
                },
                0,
                cx,
            );
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![track(99)],
                    playlists: vec![],
                },
                0,
                cx,
            );
        });
        window.dispatch_event(
            MouseUpEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    if let Ok(command) = fixture.backend.commands.try_recv() {
        panic!("unexpected stale click command: {command:?}");
    }
    fixture.update(|window, cx| {
        window.click("search-input", cx);
        window.input(" ", cx);
    });
    fixture.cx.advance_clock(Duration::from_millis(130));
    fixture.wait_for(|window, _| window.try_find("cached-search-suggestions").is_some());
    fixture.update(|window, cx| window.click("nav-favorites", cx));
    fixture.update(|window, _| assert!(window.try_find("cached-search-suggestions").is_none()));
}

#[test]
fn cached_suggestions_cancel_pending_work_when_query_is_cleared() {
    let mut fixture = cached_search_fixture();
    fixture.update(|window, cx| {
        window.input("Song", cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.press("backspace", cx);
    });
    fixture.cx.advance_clock(Duration::from_millis(300));
    fixture.update(|window, _| assert!(window.try_find("cached-search-suggestions").is_none()));
    fixture.no_commands();
}

#[test]
fn search_shortcut_focuses_input_and_enter_submits_text_without_toggling_playback() {
    let mut fixture = Fixture::new(false);
    fixture.no_commands();
    fixture.update(|window, cx| {
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-k"
            } else {
                "ctrl-k"
            },
            cx,
        )
    });
    fixture.update(|window, cx| {
        assert_eq!(window.find("search-input").focused(), Some(true));
        window.input("Blue", cx);
        window.press("space", cx);
        window.input("Train", cx);
        assert_eq!(window.find("search-input").value(), Some("Blue Train"));
    });
    fixture.no_commands();
    fixture.update(|window, cx| window.press("enter", cx));
    match fixture.backend.commands.try_recv().expect("search command") {
        BackendCommand::SearchCatalog { query, .. } => assert_eq!(query, "Blue Train"),
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
    fixture.update(|_, cx| assert!(!services::AppServices::player(cx).read(cx).playing()));
}

#[test]
fn client_id_validation_keeps_focus_and_only_submits_valid_input() {
    let mut fixture = Fixture::setup();
    fixture.update(|window, cx| {
        window.click("client-id-input", cx);
        window.input("not-a-client-id", cx);
    });
    fixture.update(|window, cx| window.press("enter", cx));
    fixture.no_commands();
    fixture.update(|window, cx| window.click("save-spotify-client-id", cx));
    fixture.update(|window, cx| {
        assert_eq!(window.find("client-id-input").focused(), Some(true));
        assert_eq!(
            window.find("client-id-input").value(),
            Some("not-a-client-id")
        );
        assert!(
            services::AppServices::session(cx)
                .read(cx)
                .setup_error()
                .is_some()
        );
    });
    fixture.no_commands();
    fixture.update(|window, cx| {
        window.click("client-id-input", cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.input("0123456789abcdef0123456789abcdef", cx);
    });
    fixture.update(|window, cx| {
        assert!(
            services::AppServices::session(cx)
                .read(cx)
                .setup_error()
                .is_none()
        );
        window.press("enter", cx);
    });
    match fixture
        .backend
        .commands
        .try_recv()
        .expect("configuration command")
    {
        BackendCommand::ConfigureSpotify { client_id, .. } => {
            assert_eq!(client_id, "0123456789abcdef0123456789abcdef");
        }
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
}

#[test]
fn autoplay_switch_updates_preferences_and_rendered_state() {
    let mut fixture = Fixture::new(true);
    fixture.update(|window, cx| {
        assert_eq!(window.find("settings-autoplay").checked(), Some(true));
        window.click("settings-autoplay", cx);
    });
    fixture.update(|window, cx| {
        assert!(!services::AppServices::preferences(cx).autoplay);
        assert_eq!(window.find("settings-autoplay").checked(), Some(false));
        window.click("settings-autoplay", cx);
    });
    fixture.update(|window, cx| {
        assert!(services::AppServices::preferences(cx).autoplay);
        assert_eq!(window.find("settings-autoplay").checked(), Some(true));
    });
    fixture.no_commands();
}

#[test]
fn tab_rings_the_autoplay_switch_and_space_toggles_it() {
    let mut fixture = Fixture::new(true);
    let workspace = fixture.workspace.clone().expect("workspace fixture");
    let autoplay_ring_visible = move |window: &Window, cx: &mut App| {
        let workspace = workspace.upgrade().expect("live workspace");
        let settings = workspace.read(cx).settings.read(cx);
        components::is_focus_visible_within(settings.autoplay_focus(), window, cx)
    };
    fixture.update(|window, cx| assert!(!autoplay_ring_visible(window, cx)));

    fixture.tab_to("settings-autoplay");
    fixture.update(|window, cx| {
        assert!(autoplay_ring_visible(window, cx));
        assert_eq!(window.find("settings-autoplay").checked(), Some(true));
    });

    fixture.press("space");
    fixture.update(|window, cx| {
        assert!(!services::AppServices::preferences(cx).autoplay);
        assert_eq!(window.find("settings-autoplay").checked(), Some(false));
        assert!(autoplay_ring_visible(window, cx));
    });
    fixture.no_commands();
}

#[test]
fn mascot_select_commits_enter_and_preserves_selection_on_escape_and_outside_click() {
    let mut fixture = Fixture::new(true);
    fixture.update(|window, cx| window.within("settings-mascot").click("input", cx));
    fixture.update(|window, cx| {
        assert_eq!(window.find("settings-mascot").expanded(), Some(true));
        window.press("down", cx);
    });
    fixture.update(|window, cx| window.press("enter", cx));
    fixture.wait_for(|window, _| window.find("settings-mascot").expanded() == Some(false));
    fixture.update(|window, cx| {
        assert_eq!(
            services::AppServices::preferences(cx).mascot,
            MascotPreference::RomeoVespa
        );
        assert_eq!(window.find("settings-mascot").value(), Some("Vespa Romeo"));
        window.within("settings-mascot").click("input", cx);
    });
    fixture.update(|window, cx| window.press("down", cx));
    fixture.update(|window, cx| window.press("escape", cx));
    fixture.wait_for(|window, _| window.find("settings-mascot").expanded() == Some(false));
    fixture.update(|window, cx| {
        assert_eq!(
            services::AppServices::preferences(cx).mascot,
            MascotPreference::RomeoVespa
        );
        assert_eq!(window.find("settings-mascot").value(), Some("Vespa Romeo"));
        window.within("settings-mascot").click("input", cx);
    });
    fixture.update(|window, cx| window.press("down", cx));
    fixture.update(|window, cx| window.click("volume", cx));
    fixture.wait_for(|window, _| window.find("settings-mascot").expanded() == Some(false));
    fixture.update(|window, cx| {
        assert_eq!(
            services::AppServices::preferences(cx).mascot,
            MascotPreference::RomeoVespa
        );
        assert_eq!(window.find("settings-mascot").value(), Some("Vespa Romeo"));
    });
}

#[test]
fn queue_toggle_and_close_button_control_the_panel_without_backend_commands() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        assert!(window.try_find("close-queue").is_none());
        window.click("queue-toggle", cx);
    });
    fixture.update(|window, cx| {
        assert!(window.find("close-queue").visible());
        window.click("queue-toggle", cx);
    });
    fixture.update(|window, cx| {
        assert!(window.try_find("close-queue").is_none());
        window.click("queue-toggle", cx);
    });
    fixture.update(|window, cx| window.click("close-queue", cx));
    fixture.update(|window, _| assert!(window.try_find("close-queue").is_none()));
    fixture.no_commands();
}

#[test]
fn queue_keyboard_activation_preserves_playback_and_reports_expansion() {
    let mut fixture = Fixture::new(false);
    fixture.press("space");
    assert!(matches!(
        fixture
            .backend
            .commands
            .try_recv()
            .expect("playback shortcut"),
        BackendCommand::Resume
    ));
    let bounds = fixture.update(|window, _| window.find("queue-toggle").bounds());
    fixture.tab_to("queue-toggle");
    fixture.update(|window, _| {
        let button = window.find("queue-toggle");
        assert_eq!(button.role(), Some(gpui_kit::Role::Button));
        assert_eq!(button.label(), Some("Queue"));
        assert_eq!(button.expanded(), Some(false));
        assert_eq!(button.bounds(), bounds);
    });

    for (key, expanded) in [("space", true), ("enter", false), ("space", true)] {
        fixture.press(key);
        fixture.update(|window, cx| {
            assert_eq!(window.find("queue-toggle").expanded(), Some(expanded));
            assert_eq!(window.find("queue-toggle").focused(), Some(true));
            assert_eq!(window.find("queue-toggle").bounds(), bounds);
            assert_eq!(window.try_find("close-queue").is_some(), expanded);
            assert!(services::AppServices::player(cx).read(cx).playing());
        });
        fixture.no_commands();
    }
}

#[test]
fn duplicate_track_actions_preserve_row_index_and_favorite_does_not_start_playback() {
    let mut fixture = Fixture::new(false);
    fixture.right_click_track(2);
    fixture.no_commands();
    fixture.update(|window, cx| {
        assert!(window.try_find(("track-menu-play", 1usize)).is_none());
        window.click(("track-menu-play", 2usize), cx);
    });
    match fixture.backend.commands.try_recv().expect("play command") {
        BackendCommand::PlayContext { tracks, index } => {
            assert_eq!(index, 2);
            assert_eq!(tracks.len(), 4);
            assert_eq!(tracks[1].source_id, tracks[2].source_id);
            assert_eq!(tracks[index].source_id, track(1).source_id);
        }
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
    fixture.update(|window, cx| window.click(("spotify-favorite", 3usize), cx));
    match fixture
        .backend
        .commands
        .try_recv()
        .expect("favorite command")
    {
        BackendCommand::SetFavorite {
            track: favorite,
            favorite: true,
        } => assert_eq!(favorite.source_id, track(2).source_id),
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LocalStateLoaded {
                    favorites: vec![track(0), track(2)],
                    pinned_playlists: Vec::new(),
                    recently_played: Vec::new(),
                },
                0,
                cx,
            );
        });
    });
    fixture.update(|window, cx| window.click(("spotify-favorite", 3usize), cx));
    match fixture
        .backend
        .commands
        .try_recv()
        .expect("unfavorite command")
    {
        BackendCommand::SetFavorite {
            track: favorite,
            favorite: false,
        } => assert_eq!(favorite.source_id, track(2).source_id),
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
}

#[test]
fn liked_songs_use_right_click_without_a_trailing_button_or_starting_playback() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        window.hover(("spotify-track", 1usize), cx);
        assert!(window.try_find(("track-actions", 1usize)).is_none());
    });
    fixture.right_click_track(1);
    fixture.no_commands();
    fixture.update(|window, _| assert!(window.try_find(("track-menu-play", 1usize)).is_some()));
    fixture.right_click_track_at(2, 40.);
    fixture.no_commands();
    fixture.update(|window, _| {
        assert!(window.try_find(("track-menu-play", 1usize)).is_none());
        assert!(window.try_find(("track-menu-play", 2usize)).is_some());
    });
    fixture.update(|window, cx| window.click("queue-toggle", cx));
    fixture.update(|window, _| assert!(window.try_find(("track-menu-play", 2usize)).is_none()));
    fixture.no_commands();
}

/// A point on the menu surface just above `item`, in the menu's own padding,
/// so a press there lands on the menu rather than on any item in it.
fn menu_padding_above(item: &ElementSnapshot) -> Point<gpui_kit::Pixels> {
    let bounds = item.bounds();
    point(bounds.center().x, bounds.origin.y - px(2.))
}

#[test]
fn pressing_inside_the_track_menu_keeps_it_open_without_playing_the_row_beneath() {
    let mut fixture = Fixture::new(false);
    fixture.right_click_track(1);
    let menu_padding =
        fixture.update(|window, _| menu_padding_above(&window.find(("track-menu-play", 1usize))));
    let row_beneath = fixture.update(|window, _| window.find(("spotify-track", 1usize)).bounds());
    assert!(row_beneath.contains(&menu_padding));

    fixture.mouse_down(menu_padding);
    fixture.mouse_up(menu_padding);

    fixture.update(|window, _| assert!(window.try_find(("track-menu-play", 1usize)).is_some()));
    fixture.no_commands();
}

#[test]
fn pressing_inside_the_account_menu_keeps_it_open_and_its_items_still_act() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| window.click("account", cx));
    let menu_padding =
        fixture.update(|window, _| menu_padding_above(&window.find("account-settings")));

    fixture.mouse_down(menu_padding);
    fixture.mouse_up(menu_padding);
    fixture.update(|window, _| assert!(window.try_find("account-settings").is_some()));

    fixture.update(|window, cx| window.click("account-settings", cx));
    assert_eq!(fixture.route(), Route::Settings);
    fixture.no_commands();
}

#[test]
fn mute_sends_volume_and_restores_the_previous_level() {
    let mut fixture = Fixture::new(false);
    let initial = fixture.update(|_, cx| services::AppServices::player(cx).read(cx).volume());
    fixture.update(|window, cx| window.click("volume", cx));
    assert_eq!(*fixture.backend.volume.borrow_and_update(), 0.);
    fixture.update(|window, cx| window.click("volume", cx));
    assert_eq!(*fixture.backend.volume.borrow_and_update(), initial);
    fixture.no_commands();
}

fn artist_ref(name: &str, source_id: &str) -> model::ArtistRef {
    model::ArtistRef {
        name: name.into(),
        source_id: Some(source_id.into()),
        spotify_uri: Some(format!("spotify:artist:{source_id}")),
    }
}

/// A track Spotify fully described, with two credited artists and an album.
pub(super) fn credited_track() -> model::Track {
    let mut track = track(0);
    track.artist = "Frankie Valli, The Four Seasons".into();
    track.artists = vec![
        artist_ref("Frankie Valli", "artist-valli"),
        artist_ref("The Four Seasons", "artist-seasons"),
    ];
    track.album_ref = Some(model::AlbumRef {
        name: "Grease".into(),
        source_id: Some("album-grease".into()),
        spotify_uri: Some("spotify:album:album-grease".into()),
        artwork_url: None,
    });
    track
}

#[test]
fn shared_rows_open_each_artist_and_album_from_every_library_collection() {
    for nav in ["nav-library", "nav-favorites", "nav-recent"] {
        for artist in [Some(0usize), Some(1), None] {
            let mut fixture = Fixture::new(false);
            fixture.update(|_, cx| {
                services::AppServices::library(cx).update(cx, |library, cx| {
                    library.handle_backend_event(
                        BackendEvent::LibraryLoaded {
                            generation: 0,
                            liked_tracks: vec![credited_track()],
                            playlists: vec![],
                        },
                        0,
                        cx,
                    );
                    library.handle_backend_event(
                        BackendEvent::LocalStateLoaded {
                            favorites: vec![credited_track()],
                            pinned_playlists: vec![],
                            recently_played: vec![credited_track()],
                        },
                        0,
                        cx,
                    );
                });
            });
            fixture.update(|window, cx| window.click(nav, cx));
            fixture.update(|window, cx| {
                let mut row = window.within(("spotify-track", 0usize));
                if let Some(artist) = artist {
                    assert_eq!(
                        row.find(("track-artist", artist)).role(),
                        Some(gpui_kit::Role::Link)
                    );
                    row.click(("track-artist", artist), cx);
                } else {
                    row.click(("track-album", 0usize), cx);
                }
            });
            if let Some(artist) = artist {
                assert_eq!(
                    fixture.requested_artist(),
                    if artist == 0 {
                        "artist-valli"
                    } else {
                        "artist-seasons"
                    }
                );
            } else {
                assert_eq!(fixture.requested_album(), "album-grease");
            }
            fixture.no_commands();
        }
    }
}

#[test]
fn row_artwork_opens_album_and_plain_legacy_credits_stay_plain() {
    let mut fixture = Fixture::at_width(900.);
    fixture.update(|window, _| {
        assert!(window.try_find(("track-artist", 0usize)).is_none());
        assert!(window.try_find(("track-artwork", 0usize)).is_none());
    });
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![credited_track()],
                    playlists: vec![],
                },
                0,
                cx,
            );
        })
    });
    fixture.update(|window, cx| {
        assert!(window.try_find(("track-album", 0usize)).is_none());
        window.click(("track-artwork", 0usize), cx);
    });
    assert_eq!(fixture.requested_album(), "album-grease");
    fixture.no_commands();
}

#[test]
fn track_artist_links_work_inside_an_album_without_playing() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.open_album(credited_track().album_ref.unwrap(), Route::LikedSongs, cx);
        })
    });
    let BackendCommand::LoadAlbum { respond, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("album request")
    };
    respond
        .send(Ok((
            model::Album {
                provider: model::Provider::Spotify,
                source_id: "album-grease".into(),
                spotify_uri: None,
                name: "Grease".into(),
                artists: credited_track().artists,
                release_date: None,
                artwork_url: None,
                track_count: Some(1),
            },
            vec![credited_track()],
        )))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.update(|window, cx| {
        assert!(
            window.try_find(("track-artwork", 0usize)).is_none(),
            "album rows must not construct redundant cover elements"
        );
        window
            .within(("spotify-track", 0usize))
            .click(("track-artist", 1usize), cx)
    });
    assert_eq!(fixture.requested_artist(), "artist-seasons");
    fixture.no_commands();
}

#[test]
fn search_track_album_links_use_loaded_metadata_without_searching_again() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        window.click("search-input", cx);
        window.input("Grease", cx);
        window.press("enter", cx);
    });
    let BackendCommand::SearchCatalog { respond, .. } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("search")
    };
    respond
        .send(Ok(model::CatalogSearchResults {
            artists: vec![],
            tracks: vec![credited_track()],
            playlists: vec![],
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.update(|window, cx| window.click(("track-album", 0usize), cx));
    assert_eq!(fixture.requested_album(), "album-grease");
    fixture.no_commands();
}

#[test]
fn track_links_work_from_keyboard_and_queue_without_playback_commands() {
    let mut fixture = Fixture::new(false);
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![credited_track()],
                    playlists: vec![],
                },
                0,
                cx,
            );
        })
    });
    fixture.tab_to(("track-artist", 1usize));
    fixture.press("enter");
    assert_eq!(fixture.requested_artist(), "artist-seasons");
    fixture.no_commands();

    let mut next = credited_track();
    next.source_id = "linked-next".into();
    fixture.player_event(BackendEvent::PlaybackSnapshotLoaded {
        current: track(0),
        next: vec![next],
        position_ms: 0,
    });
    fixture.update(|window, cx| window.click("queue-toggle", cx));
    fixture.update(|window, cx| {
        window
            .within("queue-drawer")
            .within(("spotify-track", 1usize))
            .click(("track-artist", 0usize), cx)
    });
    assert_eq!(fixture.requested_artist(), "artist-valli");
    fixture.no_commands();
}

#[test]
fn loaded_playlist_row_opens_album_without_fetching_more_tracks() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.load_playlist(paged_playlist(), cx);
            workspace.navigate(Route::Playlist, cx);
        })
    });
    let BackendCommand::LoadPlaylist { respond, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("playlist")
    };
    respond
        .send(Ok(model::PlaylistTrackPage {
            tracks: vec![credited_track()],
            source_offsets: vec![],
            next_offset: None,
            total: 1,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.update(|window, cx| window.click(("track-album", 0usize), cx));
    assert_eq!(fixture.requested_album(), "album-grease");
    fixture.no_commands();
}

#[test]
fn stale_track_row_input_cannot_favorite_a_replacement_track() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        let position = window.find(("spotify-favorite", 1usize)).bounds().center();
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![track(0), track(2)],
                    playlists: vec![],
                },
                0,
                cx,
            );
        });
        // Send queued input against the completed frame, without the test
        // click helper's intervening redraws of the replacement collection.
        window.dispatch_event(
            MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    if let Ok(command) = fixture.backend.commands.try_recv() {
        let BackendCommand::SetFavorite { track: target, .. } = command else {
            panic!("only a favorite write is allowed")
        };
        assert_eq!(
            target.source_id,
            track(1).source_id,
            "queued input must not target the replacement track"
        );
    }
    fixture.no_commands();
}

#[test]
fn pinned_playlists_stay_above_recent_items_and_keep_their_order() {
    let mut fixture = Fixture::new(false);
    let playlists: Vec<_> = (0..6)
        .map(|i| model::Playlist {
            source_id: format!("playlist-{i}"),
            ..paged_playlist()
        })
        .collect();
    fixture.update(|_, cx| {
        cx.set_reduce_motion(true);
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![track(0)],
                    playlists: playlists.clone(),
                },
                0,
                cx,
            );
            library.handle_backend_event(
                BackendEvent::LocalStateLoaded {
                    favorites: vec![],
                    pinned_playlists: vec![playlists[1].clone(), playlists[3].clone()],
                    recently_played: vec![],
                },
                0,
                cx,
            );
        });
    });
    for clicked in [4usize, 2, 1, 3, 5] {
        fixture.update(|window, cx| {
            window.click(
                (
                    if clicked == 1 || clicked == 3 {
                        "pinned-playlist"
                    } else {
                        "sidebar-playlist"
                    },
                    clicked,
                ),
                cx,
            )
        });
        let BackendCommand::LoadPlaylist { .. } = fixture.backend.commands.try_recv().unwrap()
        else {
            panic!("playlist navigation")
        };
        fixture.update(|window, _| {
            let first = window.find(("pinned-playlist", 1usize)).bounds();
            let second = window.find(("pinned-playlist", 3usize)).bounds();
            assert!(first.top() < second.top());
            for index in [0usize, 2, 4, 5] {
                assert!(second.top() < window.find(("sidebar-playlist", index)).bounds().top());
            }
        });
    }
    fixture.no_commands();
}

#[test]
fn discrete_wheel_preserves_distance_and_precise_scrolling_stays_direct() {
    fn run(smooth: bool, precise: bool) -> f32 {
        let mut fixture = Fixture::new(false);
        fixture.update(|_, cx| {
            cx.set_reduce_motion(!smooth);
            services::AppServices::library(cx).update(cx, |library, cx| {
                library.handle_backend_event(
                    BackendEvent::LibraryLoaded {
                        generation: 0,
                        liked_tracks: (0..200).map(track).collect(),
                        playlists: vec![],
                    },
                    0,
                    cx,
                );
            });
        });
        let before =
            fixture.update(|window, _| window.find(("spotify-track", 2usize)).bounds().top());
        fixture.update(|window, cx| {
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position: window.find("track-scroll-viewport").bounds().center(),
                    delta: if precise {
                        gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-60.)))
                    } else {
                        gpui_kit::ScrollDelta::Lines(point(0., -3.))
                    },
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
        });
        if smooth && !precise {
            std::thread::sleep(Duration::from_millis(16));
            fixture.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
            std::thread::sleep(Duration::from_millis(85));
            fixture.update(|window, cx| {
                window.simulate_next_frame(cx);
            });
        }
        let after =
            fixture.update(|window, _| window.find(("spotify-track", 2usize)).bounds().top());
        (before - after).into()
    }
    let native = run(false, false);
    let smooth = run(true, false);
    assert!(native > 0.);
    assert!(
        (native - smooth).abs() < 1.,
        "smoothing must preserve wheel travel: {native} vs {smooth}"
    );
    assert!(
        (run(true, true) - 60.).abs() < 1.,
        "pixel input must scroll immediately"
    );
}

#[test]
fn scrollbar_gutter_keeps_duration_columns_stable_on_hover_and_drag() {
    for (width, zoom) in [(1280., 100), (900., 125)] {
        let mut fixture = Fixture::at_width(width);
        fixture.update(|_, cx| {
            appearance::Appearance::set_ui_zoom(
                crate::storage::UiZoom::from_percent(zoom).unwrap(),
                cx,
            )
            .unwrap();
            services::AppServices::library(cx).update(cx, |library, cx| {
                library.handle_backend_event(
                    BackendEvent::LibraryLoaded {
                        generation: 0,
                        liked_tracks: (0..200).map(track).collect(),
                        playlists: vec![],
                    },
                    0,
                    cx,
                );
            });
        });
        let (duration, viewport) = fixture.update(|window, _| {
            (
                window.find(("track-duration", 0usize)).bounds(),
                window.find("track-scroll-viewport").bounds(),
            )
        });
        assert!(duration.right() <= viewport.right() - px(16.));
        fixture.update(|window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position: point(viewport.right() - px(8.), duration.center().y),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            )
        });
        fixture.update(|window, _| {
            assert_eq!(window.find(("track-duration", 0usize)).bounds(), duration)
        });
        fixture.update(|window, cx| {
            window.drag(
                point(viewport.right() - px(8.), viewport.top() + px(8.)),
                point(viewport.right() - px(8.), viewport.center().y),
                cx,
            )
        });
        fixture.update(|window, _| {
            let visible: Vec<_> = (0..200usize)
                .filter_map(|i| window.try_find(("track-duration", i)))
                .collect();
            assert!(!visible.is_empty());
            for item in visible {
                assert_eq!(item.bounds().right(), duration.right());
            }
        });
        fixture.no_commands();
    }
}

#[test]
fn player_bar_opens_each_credited_artist_and_the_album_separately() {
    let mut fixture = Fixture::new(false);
    fixture.play(credited_track());
    fixture.no_commands();

    fixture.update(|window, cx| window.click(("player-artist", 1usize), cx));
    assert_eq!(fixture.requested_artist(), "artist-seasons");
    assert_eq!(fixture.route(), Route::Artist);

    fixture.update(|window, cx| window.click(("player-artist", 0usize), cx));
    assert_eq!(fixture.requested_artist(), "artist-valli");
    assert_eq!(fixture.route(), Route::Artist);

    fixture.update(|window, cx| window.click("player-artwork", cx));
    assert_eq!(fixture.requested_album(), "album-grease");
    assert_eq!(fixture.route(), Route::Album);

    // Dropping the album reply above failed that load, so the title retries it.
    fixture.update(|window, cx| window.click("player-title", cx));
    assert_eq!(fixture.requested_album(), "album-grease");
    assert_eq!(fixture.route(), Route::Album);
    fixture.no_commands();
}

#[test]
fn player_bar_title_opens_the_album_from_the_keyboard() {
    let mut fixture = Fixture::new(false);
    fixture.play(credited_track());
    fixture.press(if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    });
    assert_eq!(fixture.route(), Route::Search);

    fixture.tab_to("player-title");
    fixture.update(|window, _| {
        assert_eq!(
            window.find("player-title").role(),
            Some(gpui_kit::Role::Link)
        );
    });
    fixture.press("enter");
    assert_eq!(fixture.requested_album(), "album-grease");
    assert_eq!(fixture.route(), Route::Album);
    fixture.no_commands();
}

#[test]
fn player_bar_credits_without_references_stay_plain() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, _| {
        assert!(window.try_find("player-artwork").is_none());
        assert!(window.try_find("player-title").is_none());
        assert!(window.try_find(("player-artist", 0usize)).is_none());
    });
    fixture.no_commands();
}

#[test]
fn unavailable_live_track_keeps_the_queue_and_skips_ahead() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::Playing {
        spotify_uri: "spotify:track:track-0".into(),
    });
    assert_eq!(fixture.marked_played(), "track-0");
    fixture.player_event(BackendEvent::TrackFailed {
        spotify_uri: "spotify:track:track-0".into(),
        error: "Spotify cannot play this track".into(),
    });
    fixture.update(|_, cx| {
        let player = services::AppServices::player(cx).read(cx);
        assert_eq!(
            player.now_playing().map(|track| track.source_id.as_str()),
            Some("track-0")
        );
        assert!(!player.playing());
    });
    assert_eq!(fixture.queued_source_ids(), ["track-1", "track-2"]);
    assert!(matches!(
        fixture.backend.commands.try_recv(),
        Ok(BackendCommand::Next)
    ));
    fixture.no_commands();
}

#[test]
fn stale_track_failure_does_not_skip_ahead() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::TrackFailed {
        spotify_uri: "spotify:track:track-9".into(),
        error: "Spotify cannot play this track".into(),
    });
    assert_eq!(fixture.queued_source_ids(), ["track-1", "track-2"]);
    fixture.no_commands();
}

#[test]
fn track_failure_during_reconnect_does_not_skip_ahead() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackReconnecting);
    fixture.player_event(BackendEvent::TrackFailed {
        spotify_uri: "spotify:track:track-0".into(),
        error: "Spotify cannot play this track".into(),
    });
    assert_eq!(fixture.queued_source_ids(), ["track-1", "track-2"]);
    fixture.no_commands();
}

#[test]
fn history_is_recorded_once_the_track_plays_and_only_once() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: vec![track(4)],
    });
    fixture.no_commands();
    fixture.player_event(BackendEvent::Playing {
        spotify_uri: "spotify:track:track-3".into(),
    });
    assert_eq!(fixture.marked_played(), "track-3");
    fixture.player_event(BackendEvent::Playing {
        spotify_uri: "spotify:track:track-3".into(),
    });
    fixture.no_commands();
}

#[test]
fn a_track_that_fails_to_play_is_not_recorded() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: vec![track(4)],
    });
    fixture.player_event(BackendEvent::TrackFailed {
        spotify_uri: "spotify:track:track-3".into(),
        error: "Spotify cannot play this track".into(),
    });
    assert!(matches!(
        fixture.backend.commands.try_recv(),
        Ok(BackendCommand::Next)
    ));
    fixture.no_commands();
}

#[test]
fn a_track_resumed_after_restart_is_recorded_when_it_plays() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::Playing {
        spotify_uri: "spotify:track:track-0".into(),
    });
    assert_eq!(fixture.marked_played(), "track-0");
    fixture.no_commands();
}

#[test]
fn a_reconnect_mid_track_records_history_once() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: vec![track(4)],
    });
    fixture.player_event(BackendEvent::PlaybackReconnecting);
    fixture.player_event(BackendEvent::Playing {
        spotify_uri: "spotify:track:track-3".into(),
    });
    fixture.no_commands();
    fixture.player_event(BackendEvent::PlaybackReconnected);
    assert!(matches!(
        fixture.backend.commands.try_recv(),
        Ok(BackendCommand::RestorePlayback { .. })
    ));
    fixture.player_event(BackendEvent::PlaybackRestored {
        position_ms: 0,
        playing: true,
    });
    fixture.player_event(BackendEvent::Playing {
        spotify_uri: "spotify:track:track-3".into(),
    });
    assert_eq!(fixture.marked_played(), "track-3");
    fixture.player_event(BackendEvent::Playing {
        spotify_uri: "spotify:track:track-3".into(),
    });
    fixture.no_commands();
}

/// The whole point of the scrub gesture: the position follows the pointer, and
/// the backend hears about it once, when the gesture ends.
#[test]
fn scrubbing_seeks_once_on_release_at_the_position_under_the_pointer() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: Vec::new(),
    });
    fixture.no_commands();

    let bounds = fixture.update(|window, _| window.find("progress-slider").bounds());
    let at = |fraction: f32| {
        point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        )
    };
    fixture.update(|window, cx| window.drag(at(0.25), at(0.75), cx));

    // track(3) runs 240s, so releasing three quarters along asks for 3:00.
    match fixture.backend.commands.try_recv().expect("a seek") {
        BackendCommand::Seek(position_ms) => assert!(
            position_ms.abs_diff(180_000) < 2_000,
            "expected roughly 180000ms, got {position_ms}"
        ),
        other => panic!("expected a seek, got {other:?}"),
    }
    fixture.no_commands();
}

/// The seek maps through the track's painted bounds, so it stays correct when
/// the window is a size the layout constants were never written for.
#[test]
fn scrubbing_maps_through_the_painted_track_at_any_window_width() {
    for width in [1280., 1000., 760.] {
        let mut fixture = Fixture::at_width(width);
        fixture.player_event(BackendEvent::PlaybackContext {
            current: track(3),
            next: Vec::new(),
        });
        fixture.no_commands();

        let bounds = fixture.update(|window, _| window.find("progress-slider").bounds());
        let middle = point(bounds.origin.x + bounds.size.width * 0.5, bounds.center().y);
        fixture.update(|window, cx| window.drag(middle, middle, cx));

        match fixture.backend.commands.try_recv().expect("a seek") {
            BackendCommand::Seek(position_ms) => assert!(
                position_ms.abs_diff(120_000) < 2_000,
                "at width {width}: expected roughly 120000ms, got {position_ms}"
            ),
            other => panic!("expected a seek, got {other:?}"),
        }
    }
}

#[test]
fn arrow_keys_seek_once_the_scrubber_has_focus() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: Vec::new(),
    });
    fixture.no_commands();

    fixture.update(|window, cx| window.click("progress-slider", cx));
    fixture.backend.commands.try_recv().expect("the click seek");
    fixture.update(|window, cx| window.press("right", cx));

    match fixture.backend.commands.try_recv().expect("a seek") {
        BackendCommand::Seek(position_ms) => assert!(
            position_ms.abs_diff(125_000) < 2_000,
            "expected roughly 125000ms, got {position_ms}"
        ),
        other => panic!("expected a seek, got {other:?}"),
    }
}

/// Escape during a scrub abandons it. No seek was ever sent, so there is
/// nothing to undo: commanding one would rewind playback by the drag's length.
#[test]
fn escape_during_a_scrub_abandons_it_without_seeking() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: Vec::new(),
    });
    fixture.no_commands();

    let bounds = fixture.update(|window, _| window.find("progress-slider").bounds());
    let at = |fraction: f32| {
        point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        )
    };
    fixture.mouse_down(at(0.25));
    fixture.mouse_move_to(at(0.75));
    fixture.press("escape");
    fixture.no_commands();

    fixture.mouse_up(at(0.75));
    fixture.no_commands();
}

/// A gesture belongs to the track it began on. If that track goes away the
/// press must not commit against whatever is playing by the time it is released.
#[test]
fn a_scrub_whose_track_ends_mid_gesture_does_not_seek_the_next_one() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: Vec::new(),
    });
    fixture.no_commands();

    let bounds = fixture.update(|window, _| window.find("progress-slider").bounds());
    let at = |fraction: f32| {
        point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        )
    };
    fixture.mouse_down(at(0.25));
    fixture.mouse_move_to(at(0.75));

    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(7),
        next: Vec::new(),
    });
    fixture.mouse_up(at(0.75));
    fixture.no_commands();
}

/// Guards the split press/move/release helpers the cancel tests rely on: the
/// preview has to actually follow the pointer, or those tests would pass for
/// the wrong reason. gpui's first qualifying move only opens the drag, so the
/// preview tracks from the second move onwards.
#[test]
fn a_scrub_preview_follows_the_pointer_before_it_commits() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: Vec::new(),
    });
    fixture.no_commands();

    let bounds = fixture.update(|window, _| window.find("progress-slider").bounds());
    let at = |fraction: f32| {
        point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        )
    };
    fixture.mouse_down(at(0.25));
    fixture.mouse_move_to(at(0.5));
    fixture.mouse_move_to(at(0.75));
    fixture.no_commands();
    fixture.mouse_up(at(0.75));

    match fixture.backend.commands.try_recv().expect("a seek") {
        BackendCommand::Seek(position_ms) => assert!(
            position_ms.abs_diff(180_000) < 2_000,
            "expected roughly 180000ms, got {position_ms}"
        ),
        other => panic!("expected a seek, got {other:?}"),
    }
}

/// End means the end. Landing there finishes the track and moves on, which is
/// what every other player does.
#[test]
fn seeking_to_the_end_lands_on_the_end() {
    let mut fixture = Fixture::new(false);
    fixture.player_event(BackendEvent::PlaybackContext {
        current: track(3),
        next: Vec::new(),
    });
    fixture.no_commands();

    fixture.update(|window, cx| window.click("progress-slider", cx));
    fixture.backend.commands.try_recv().expect("the click seek");
    fixture.press("end");

    match fixture.backend.commands.try_recv().expect("a seek") {
        BackendCommand::Seek(position_ms) => assert_eq!(position_ms, 240_000),
        other => panic!("expected a seek, got {other:?}"),
    }
}

#[test]
fn closing_the_main_window_releases_the_workspace_while_services_keep_emitting() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().expect("workspace fixture");
    fixture
        .cx
        .update_window(fixture.window.into(), |_, window, _| window.remove_window())
        .expect("fixture window");
    fixture.cx.run_until_parked();
    fixture.cx.update(|cx| {
        assert!(workspace.upgrade().is_none());
        services::AppServices::session(cx).update(cx, |session, cx| {
            session.handle_backend_event(BackendEvent::SetupRequired, cx);
        });
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![track(0)],
                    playlists: Vec::new(),
                },
                0,
                cx,
            );
        });
    });
    fixture.cx.run_until_parked();
}

#[test]
fn reduced_motion_toggles_the_sidebar_straight_to_its_final_width() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().expect("workspace fixture");
    let sidebar_width = move |cx: &mut App| {
        let workspace = workspace.upgrade().expect("live workspace");
        workspace.read(cx).sidebar.read(cx).rendered_width()
    };
    // The collapsed rail clips the toggle, so press where its logo stays visible.
    let logo = point(px(20.), px(20.));
    fixture.update(|window, cx| {
        cx.set_reduce_motion(true);
        assert_eq!(sidebar_width(cx), EXPANDED_SIDEBAR_WIDTH);
        window.click_at("sidebar-toggle", logo, cx);
    });
    let collapsed_width = fixture.update(|_, cx| sidebar_width(cx));
    assert_eq!(collapsed_width, COLLAPSED_SIDEBAR_WIDTH);

    fixture.update(|window, cx| window.click_at("sidebar-toggle", logo, cx));
    let expanded_width = fixture.update(|_, cx| sidebar_width(cx));
    assert_eq!(expanded_width, EXPANDED_SIDEBAR_WIDTH);
}

#[test]
fn tab_reaches_the_sidebar_rows_and_enter_opens_the_focused_row() {
    let mut fixture = Fixture::new(false);
    fixture.tab_to("nav-favorites");

    fixture.press("enter");
    assert_eq!(fixture.route(), Route::Favorites);
    assert!(fixture.focused("nav-favorites"));
    fixture.no_commands();
}

#[test]
fn space_activates_a_focused_transport_control_instead_of_toggling_playback() {
    let mut fixture = Fixture::new(false);
    fixture.tab_to("progress-slider");
    fixture.press("shift-tab");

    fixture.press("space");
    assert!(matches!(
        fixture.backend.commands.try_recv().expect("next track"),
        BackendCommand::Next
    ));
    fixture.no_commands();
}

#[test]
fn a_pointer_press_leaves_keyboard_focus_with_the_workspace() {
    let mut fixture = Fixture::new(false);
    fixture.press(if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    });
    assert!(fixture.focused("search-input"));

    fixture.update(|window, cx| window.click("nav-favorites", cx));
    assert_eq!(fixture.route(), Route::Favorites);
    assert!(!fixture.focused("nav-favorites"));
    assert!(!fixture.focused("search-input"));

    fixture.press("space");
    assert!(matches!(
        fixture
            .backend
            .commands
            .try_recv()
            .expect("playback shortcut"),
        BackendCommand::Resume
    ));
    fixture.no_commands();
}

#[test]
fn the_play_button_and_space_drive_the_same_playback_toggle() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| window.click("play-toggle", cx));
    assert!(matches!(
        fixture.backend.commands.try_recv().expect("play button"),
        BackendCommand::Resume
    ));
    fixture.update(|_, cx| assert!(services::AppServices::player(cx).read(cx).playing()));

    fixture.press("space");
    assert!(matches!(
        fixture
            .backend
            .commands
            .try_recv()
            .expect("playback shortcut"),
        BackendCommand::Pause
    ));
    fixture.update(|_, cx| assert!(!services::AppServices::player(cx).read(cx).playing()));
    fixture.no_commands();
}

#[test]
fn escape_cancels_the_sign_in_app_change_wherever_keyboard_focus_is() {
    for tab_presses in 0..10 {
        let mut fixture = Fixture::sign_in_window();
        fixture.update(|_, cx| {
            services::AppServices::session(cx)
                .update(cx, |session, cx| session.request_app_change(cx));
        });
        for _ in 0..tab_presses {
            fixture.press("tab");
        }
        fixture.press("escape");
        let open = fixture.app_change_confirmation_open();
        assert!(
            !open,
            "Escape left the confirmation open after {tab_presses} Tab presses"
        );
        fixture.no_commands();
    }
}

#[test]
fn empty_artist_still_has_both_sections_after_skeleton_loading() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.open_artist(
                artist_ref("Empty artist", "empty-artist"),
                Route::LikedSongs,
                cx,
            );
        })
    });
    let BackendCommand::LoadArtist { respond, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("artist request")
    };
    respond
        .send(Ok((
            model::Artist {
                provider: model::Provider::Spotify,
                source_id: "empty-artist".into(),
                spotify_uri: None,
                name: "Empty artist".into(),
                artwork_url: None,
            },
            vec![],
            vec![],
        )))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.update(|window, _| {
        assert!(window.try_find("artist-popular-heading").is_some());
        assert!(window.try_find("artist-discography-heading").is_some());
    });
}

#[test]
fn artist_tracks_scroll_into_virtualized_discography_and_albums_still_open() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        cx.set_reduce_motion(true);
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.open_artist(artist_ref("Artist", "artist-flow"), Route::LikedSongs, cx);
        });
    });
    let BackendCommand::LoadArtist { respond, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected artist request");
    };
    respond
        .send(Ok((
            model::Artist {
                provider: model::Provider::Spotify,
                source_id: "artist-flow".into(),
                spotify_uri: None,
                name: "Artist".into(),
                artwork_url: None,
            },
            vec![track(0), track(1), track(2)],
            (0..400)
                .map(|index| model::Album {
                    provider: model::Provider::Spotify,
                    source_id: format!("album-{index}"),
                    spotify_uri: None,
                    name: format!("Album {index}").into(),
                    artists: Vec::new(),
                    release_date: None,
                    artwork_url: None,
                    track_count: Some(10),
                })
                .collect(),
        )))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.update(|window, cx| {
        let last_track = window.find(("spotify-track", 2usize)).bounds();
        assert!(window.find("artist-discography-heading").bounds().origin.y >= last_track.bottom());
        assert!(
            window.try_find(("artist-album", 399usize)).is_none(),
            "offscreen albums stay virtualized"
        );
        let position = window.find("artist-portrait").bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-450.))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.update(|window, cx| {
        let position = point(px(700.), px(400.));
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(450.))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.update(|window, cx| {
        let portrait = window.find("artist-portrait").bounds();
        assert_eq!(
            portrait.size.width,
            gpui_kit::Rems(10.).to_pixels(window.rem_size())
        );
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position: portrait.center(),
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-450.))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.update(|_, cx| {
        appearance::Appearance::set_ui_zoom(crate::storage::UiZoom::from_percent(110).unwrap(), cx)
            .unwrap();
    });
    fixture.update(|window, _| {
        assert!(
            window.find("artist-portrait").bounds().size.width
                < gpui_kit::Rems(3.).to_pixels(window.rem_size()),
            "zoom must preserve the scrolled page instead of resetting to its header"
        );
    });
    let visible_album = fixture.update(|window, cx| {
        let top = crate::app::tokens::TOOLBAR_HEIGHT.to_pixels(window.rem_size());
        let portrait = window.find("artist-portrait").bounds();
        let search = window.find("search-field").bounds();
        let account = window.find("account").bounds();
        assert!(
            (portrait.size.width - gpui_kit::Rems(2.).to_pixels(window.rem_size())).abs() < px(1.)
        );
        assert!(portrait.left() >= search.right());
        assert!(window.find("artist-moving-name").bounds().right() <= account.left());
        let index = (0usize..40)
            .find(|index| {
                window
                    .try_find(("artist-album", *index))
                    .is_some_and(|card| {
                        card.bounds().top() >= top
                            && card.bounds().bottom() < window.viewport_size().height - px(100.)
                    })
            })
            .expect("one complete discography card is visible after scrolling");
        window.click(("artist-album", index), cx);
        index
    });
    assert_eq!(fixture.requested_album(), format!("album-{visible_album}"));
    assert_eq!(fixture.route(), Route::Album);
}

#[test]
fn tracks_use_double_click_and_liked_songs_have_no_column_titles() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        assert!(window.try_find("track-list-header").is_none());
        window.click(("spotify-track", 1usize), cx);
    });
    fixture.no_commands();
    fixture.update(|window, cx| window.double_click(("spotify-track", 1usize), cx));
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::PlayContext { index: 1, .. }
    ));
    fixture.no_commands();
}

fn paged_playlist() -> model::Playlist {
    model::Playlist { provider: model::Provider::Spotify, source_id: "large-playlist".into(), name: "A very long playlist name that should stay inside the sidebar without widening the window".into(), owner: "Owner".into(), track_count: 1000, artwork_url: None }
}

#[test]
fn playlist_fetches_small_pages_only_near_the_scroll_end_and_preserves_playback_cursor() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.load_playlist(paged_playlist(), cx);
            workspace.navigate(Route::Playlist, cx);
        })
    });
    let BackendCommand::LoadPlaylist {
        offset, respond, ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected first page");
    };
    assert_eq!(offset, 0);
    respond
        .send(Ok(model::PlaylistTrackPage {
            source_offsets: vec![],
            tracks: (0..15).map(track).collect(),
            next_offset: Some(15),
            total: 1000,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.no_commands();
    fixture.update(|window, cx| window.click(("spotify-track", 1usize), cx));
    fixture.no_commands();
    fixture.update(|window, cx| window.double_click(("spotify-track", 1usize), cx));
    match fixture.backend.commands.try_recv().unwrap() {
        BackendCommand::PlayPlaylistContext {
            tracks,
            index,
            next_offset,
            ..
        } => {
            assert_eq!(tracks.len(), 15);
            assert_eq!(index, 1);
            assert_eq!(next_offset, Some(15));
        }
        _ => panic!("expected paged playback context"),
    }
    fixture.update(|window, cx| {
        let position = window.find(("spotify-track", 1usize)).bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-850.))),
                modifiers: Default::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.no_commands();
    fixture.cx.advance_clock(Duration::from_millis(100));
    fixture.update(|_, _| {});
    let BackendCommand::LoadPlaylist {
        offset, respond, ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected next page");
    };
    assert_eq!(offset, 15);
    fixture.no_commands(); // further frames/scrolls cannot duplicate an in-flight request
    respond
        .send(Ok(model::PlaylistTrackPage {
            source_offsets: vec![],
            tracks: (15..30).map(track).collect(),
            next_offset: None,
            total: 30,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.no_commands();
}

#[test]
fn playlist_keeps_raw_offsets_for_unavailable_rows_and_retries_visible_pages() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.load_playlist(paged_playlist(), cx);
            workspace.navigate(Route::Playlist, cx);
        })
    });
    let BackendCommand::LoadPlaylist { respond, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    respond
        .send(Ok(model::PlaylistTrackPage {
            source_offsets: vec![0, 2, 5],
            tracks: (0..3).map(track).collect(),
            next_offset: Some(15),
            total: 40,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.no_commands();
    // The taller detail header leaves fewer rows initially visible.
    fixture.update(|window, cx| {
        let position = window.find(("spotify-track", 0usize)).bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-200.))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.update(|window, cx| window.double_click(("spotify-track", 5usize), cx));
    let BackendCommand::PlayPlaylistContext {
        index,
        next_offset,
        tracks,
        ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!((index, next_offset, tracks.len()), (2, Some(15), 3));
    fixture.update(|window, cx| {
        let position = window.find(("spotify-track", 5usize)).bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-650.))),
                modifiers: Default::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.cx.advance_clock(Duration::from_millis(100));
    fixture.update(|_, _| {});
    let BackendCommand::LoadPlaylist {
        offset, respond, ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(offset, 15);
    respond.send(Err(anyhow::anyhow!("offline"))).unwrap();
    fixture.update(|_, _| {});
    fixture.cx.advance_clock(Duration::from_secs(1));
    fixture.update(|_, _| {});
    fixture.no_commands(); // an error banner resizing the viewport must not trigger retries
    fixture.update(|window, cx| window.click("playlist-retry-page", cx));
    fixture.cx.advance_clock(Duration::from_millis(100));
    fixture.update(|_, _| {});
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::LoadPlaylist { offset: 15, .. }
    ));
}

#[test]
fn playlist_scroll_jumps_directly_to_the_final_viewport_after_debounce() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.load_playlist(
                model::Playlist {
                    track_count: 100_000,
                    ..paged_playlist()
                },
                cx,
            );
            workspace.navigate(Route::Playlist, cx);
        })
    });
    let BackendCommand::LoadPlaylist { respond, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    respond
        .send(Ok(model::PlaylistTrackPage {
            source_offsets: vec![],
            tracks: (0..15).map(track).collect(),
            next_offset: Some(15),
            total: 100_000,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    let position =
        fixture.update(|window, _| window.find(("spotify-track", 1usize)).bounds().center());
    for delta in [-100_000_000., 100_000_000., -100_000_000.] {
        fixture.update(|window, cx| {
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position,
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(delta))),
                    modifiers: Default::default(),
                    touch_phase: gpui_kit::TouchPhase::Moved,
                }
                .to_platform_input(),
                cx,
            )
        });
    }
    fixture.update(|_, _| {});
    fixture.cx.advance_clock(Duration::from_millis(99));
    fixture.update(|_, _| {});
    fixture.no_commands();
    fixture.cx.advance_clock(Duration::from_millis(1));
    fixture.update(|_, _| {});
    let BackendCommand::LoadPlaylist {
        offset, respond, ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("visible page request")
    };
    assert_eq!(offset, 99_990);
    respond
        .send(Ok(model::PlaylistTrackPage {
            source_offsets: vec![],
            tracks: (99_990..100_000).map(track).collect(),
            next_offset: None,
            total: 100_000,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.update(|window, cx| window.double_click(("spotify-track", 99_999usize), cx));
    let BackendCommand::PlayPlaylistContext {
        tracks,
        index,
        next_offset,
        ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!((tracks.len(), index, next_offset), (10, 9, None));
    fixture.no_commands();
}

#[test]
fn selection_color_setting_applies_immediately_and_persists() {
    let mut fixture = Fixture::new(true);
    fixture.update(|window, cx| {
        window.click(("settings-selection-color", 3usize), cx);
    });
    fixture.update(|_, cx| {
        assert_eq!(
            services::AppServices::preferences(cx).selection_color,
            crate::storage::SelectionColor::Purple
        );
        assert_eq!(
            appearance::Appearance::palette(cx).selection,
            crate::storage::SelectionColor::Purple.fill(true)
        );
    });
    fixture.no_commands();
}

#[test]
fn queue_uses_shared_rows_and_double_click_keeps_the_existing_playback_context() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| window.click("queue-toggle", cx));
    fixture.update(|window, cx| {
        assert!(
            window
                .within("queue-drawer")
                .try_find(("track-actions", 1usize))
                .is_none()
        );
        window
            .within("queue-drawer")
            .click(("spotify-track", 1usize), cx);
    });
    fixture.no_commands();
    fixture.update(|window, cx| {
        window
            .within("queue-drawer")
            .double_click(("spotify-track", 1usize), cx)
    });
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::PlayQueueItem { offset: 1 }
    ));
    fixture.no_commands();
}

#[test]
fn sidebar_shows_unpinned_playlists_truncates_names_and_virtualizes_long_libraries() {
    let mut fixture = Fixture::new(false);
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![track(0)],
                    playlists: (0..200)
                        .map(|index| model::Playlist {
                            source_id: format!("playlist-{index}"),
                            ..paged_playlist()
                        })
                        .collect(),
                },
                0,
                cx,
            );
        });
    });
    fixture.update(|window, cx| {
        let row = window.find(("sidebar-playlist", 0usize)).bounds();
        assert!(row.size.width < px(240.));
        assert!(row.right() < px(240.));
        assert!(window.try_find(("sidebar-playlist", 199usize)).is_none());
        window.click(("sidebar-playlist", 0usize), cx);
    });
    let BackendCommand::LoadPlaylist {
        playlist, offset, ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected playlist navigation");
    };
    assert_eq!(playlist.source_id, "playlist-0");
    assert_eq!(offset, 0);
}

#[test]
fn current_track_index_toggles_playback_without_replacing_the_queue() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        assert_eq!(
            window.find(("track-playback-toggle", 0usize)).label(),
            Some("Play")
        );
        assert!(window.try_find(("track-playback-toggle", 1usize)).is_none());
        window.click(("track-playback-toggle", 0usize), cx);
    });
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::Resume
    ));
    fixture.update(|window, cx| {
        assert_eq!(
            window.find(("track-playback-toggle", 0usize)).label(),
            Some("Pause")
        );
        window.click(("track-playback-toggle", 0usize), cx);
    });
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::Pause
    ));
    fixture.update(|window, cx| window.double_click(("track-playback-toggle", 0usize), cx));
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::Resume
    ));
    fixture.no_commands();
    fixture.update(|window, cx| window.click("queue-toggle", cx));
    fixture.update(|window, cx| {
        window
            .within("queue-drawer")
            .click(("track-playback-toggle", 0usize), cx)
    });
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::Pause
    ));
    fixture.no_commands();
}

#[test]
fn zoom_scales_layout_and_transport_and_survives_theme_changes() {
    let mut fixture = Fixture::new(true);
    let original = fixture.update(|window, _| window.find("play-toggle").bounds().size);
    fixture.update(|window, cx| window.click(("settings-ui-zoom", 125usize), cx));
    fixture.update(|window, cx| {
        assert_eq!(window.rem_size(), px(20.));
        assert_eq!(
            services::AppServices::preferences(cx).ui_zoom.percent(),
            125
        );
        assert_eq!(
            window.find("play-toggle").bounds().size.width,
            original.width * 1.25
        );
        appearance::Appearance::set_preference(ThemePreference::Light, window, cx);
    });
    fixture.update(|window, cx| {
        assert_eq!(window.rem_size(), px(20.));
        window.click(("settings-ui-zoom", 75usize), cx);
    });
    fixture.update(|window, cx| {
        assert_eq!(window.rem_size(), px(12.));
        assert_eq!(
            window.find("play-toggle").bounds().size.width,
            original.width * 0.75
        );
        window.click(("settings-ui-zoom", 100usize), cx);
    });
    fixture.update(|window, _| assert_eq!(window.rem_size(), px(16.)));
    fixture.no_commands();
}

#[test]
fn collapsed_sidebar_keeps_playlist_buttons_inside_rail_and_promotes_opened_playlist() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![track(0)],
                    playlists: (0..200)
                        .map(|i| model::Playlist {
                            source_id: format!("playlist-{i}"),
                            ..paged_playlist()
                        })
                        .collect(),
                },
                0,
                cx,
            );
        });
    });
    fixture.update(|window, cx| window.click(("sidebar-playlist", 2usize), cx));
    let BackendCommand::LoadPlaylist { playlist, .. } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected playlist request")
    };
    assert_eq!(playlist.source_id, "playlist-2");
    fixture.update(|window, cx| {
        assert!(
            window.find(("sidebar-playlist", 2usize)).bounds().top()
                < window.find(("sidebar-playlist", 0usize)).bounds().top()
        );
        workspace
            .upgrade()
            .unwrap()
            .read(cx)
            .sidebar
            .clone()
            .update(cx, |sidebar, cx| sidebar.set_collapsed(true, cx));
    });
    fixture.update(|window, cx| {
        assert_eq!(
            window.find(("sidebar-playlist", 2usize)).selected(),
            Some(true)
        );
        assert_eq!(window.find("nav-playlist").selected(), Some(false));
        assert!(
            window.find("nav-library").bounds().top()
                <= window.find("sidebar-toggle").bounds().bottom() + px(20.)
        );
        let rail_right = COLLAPSED_SIDEBAR_WIDTH.to_pixels(window.rem_size());
        for id in ["nav-library", "nav-favorites", "nav-playlist", "nav-recent"] {
            assert!(window.find(id).bounds().right() <= rail_right);
        }
        let cover = window.find(("sidebar-playlist", 2usize)).bounds();
        assert!(cover.right() <= rail_right);
        assert!(window.try_find(("sidebar-playlist", 199usize)).is_none());
        window.click(("sidebar-playlist", 1usize), cx);
    });
    let BackendCommand::LoadPlaylist { playlist, .. } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("expected collapsed playlist navigation")
    };
    assert_eq!(playlist.source_id, "playlist-1");
}

#[test]
fn clearing_artwork_cache_releases_entries_without_changing_account_or_queue() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        let cache = services::AppServices::image_cache(cx);
        cache.update(cx, |cache, _| {
            let image = image::RgbaImage::new(32, 32);
            let image = Arc::new(gpui_kit::RenderImage::new([image::Frame::new(image)]));
            cache.seed_artist_fixture("https://fixtures.invalid/cache.png", image);
        });
        assert!(cache.read(cx).usage().0 > 0);
        let player = services::AppServices::player(cx);
        let before = player.read(cx).is_current_track(&track(0));
        cache.update(cx, |cache, cx| cache.clear(window, cx));
        assert_eq!(cache.read(cx).usage().0, 0);
        assert_eq!(player.read(cx).is_current_track(&track(0)), before);
    });
    fixture.no_commands();
}

#[test]
fn update_badge_opens_settings_without_opening_account_menu() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace
            .upgrade()
            .unwrap()
            .read(cx)
            .toolbar
            .clone()
            .update(cx, |toolbar, cx| toolbar.seed_update_available(cx))
    });
    fixture.update(|window, cx| window.click("update-available", cx));
    assert_eq!(fixture.route(), Route::Settings);
    fixture.update(|window, _| assert!(window.try_find("account-menu").is_none()));
    fixture.no_commands();
}

#[cfg(target_os = "windows")]
#[test]
fn windows_brand_and_search_share_the_same_vertical_center() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, _| {
        assert_eq!(
            window.find("sidebar-toggle").bounds().center().y,
            window.find("search-field").bounds().center().y
        )
    });
}

#[test]
fn settings_cache_cleanup_button_is_reachable_and_does_not_send_playback_commands() {
    let mut fixture = Fixture::new(true);
    fixture.update(|window, cx| {
        let position = window.find("settings-page").bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-3000.))),
                modifiers: Default::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.update(|window, cx| window.click("settings-clear-cache", cx));
    assert_eq!(fixture.route(), Route::Settings);
    fixture.no_commands();
}

#[test]
fn volume_slider_uses_live_viewport_after_fullscreen_and_resize() {
    let mut fixture = Fixture::new(false);
    for (width, fraction) in [(1920., 0.25f32), (2560., 0.75), (1280., 0.5)] {
        fixture.update(|window, _| {
            window.toggle_fullscreen();
            window.resize(size(px(width), px(1080.)));
        });
        fixture.update(|window, cx| {
            let slider = window.find("volume-slider").bounds();
            let position = point(
                slider.left() + slider.size.width * fraction,
                slider.center().y,
            );
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
        });
        assert!((*fixture.backend.volume.borrow_and_update() - fraction).abs() < 0.02);
        fixture.update(|window, cx| {
            let position = window.find("volume-slider").bounds().center();
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
        });
        fixture.no_commands();
    }
}

#[test]
fn shuffle_toggle_is_lightweight_and_preserves_transport_bounds_and_playback() {
    let mut fixture = Fixture::new(false);
    let before = fixture.update(|window, cx| {
        let player = services::AppServices::player(cx);
        assert!(!player.read(cx).shuffle());
        (
            window.find("play-toggle").bounds(),
            player.read(cx).now_playing().cloned(),
        )
    });
    fixture.update(|window, cx| window.click("shuffle", cx));
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::SetShuffle(true)
    ));
    fixture.update(|window, cx| {
        let player = services::AppServices::player(cx);
        assert!(player.read(cx).shuffle());
        assert_eq!(player.read(cx).now_playing(), before.1.as_ref());
        assert_eq!(window.find("play-toggle").bounds(), before.0);
        window.click("shuffle", cx);
    });
    assert!(matches!(
        fixture.backend.commands.try_recv().unwrap(),
        BackendCommand::SetShuffle(false)
    ));
    fixture.no_commands();
}

#[test]
fn search_places_artists_above_tracks_and_opens_the_selected_artist() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        window.click("search-input", cx);
        window.input("glaive", cx);
        window.press("enter", cx);
    });
    let BackendCommand::SearchCatalog { respond, .. } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("search request")
    };
    respond
        .send(Ok(model::CatalogSearchResults {
            artists: vec![model::Artist {
                provider: model::Provider::Spotify,
                source_id: "glaive-id".into(),
                spotify_uri: None,
                name: "glaive".into(),
                artwork_url: None,
            }],
            tracks: vec![track(0)],
            playlists: vec![],
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.update(|window, cx| {
        assert!(
            window.find(("search-artist", 0usize)).bounds().bottom()
                <= window.find(("spotify-track", 0usize)).bounds().top()
        );
        window.click(("search-artist", 0usize), cx);
    });
    assert_eq!(fixture.requested_artist(), "glaive-id");
    fixture.no_commands();
}

#[test]
fn returning_to_a_recent_playlist_restores_loaded_tracks_and_scroll_without_fetching() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    let open = |fixture: &mut Fixture, id: &str| {
        fixture.update(|_, cx| {
            workspace.upgrade().unwrap().update(cx, |workspace, cx| {
                workspace.load_playlist(
                    model::Playlist {
                        source_id: id.into(),
                        ..paged_playlist()
                    },
                    cx,
                );
                workspace.navigate(Route::Playlist, cx);
            })
        });
    };
    let answer = |fixture: &mut Fixture| {
        let BackendCommand::LoadPlaylist {
            offset, respond, ..
        } = fixture.backend.commands.try_recv().unwrap()
        else {
            panic!("playlist request")
        };
        assert_eq!(offset, 0);
        respond
            .send(Ok(model::PlaylistTrackPage {
                source_offsets: vec![],
                tracks: (0..40).map(track).collect(),
                next_offset: None,
                total: 40,
            }))
            .unwrap();
        fixture.update(|_, _| {});
    };
    open(&mut fixture, "a");
    answer(&mut fixture);
    fixture.update(|window, cx| {
        let position = window.find(("spotify-track", 1usize)).bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-850.))),
                modifiers: Default::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    let before = fixture.update(|window, _| window.find(("spotify-track", 15usize)).bounds());
    open(&mut fixture, "b");
    answer(&mut fixture);
    open(&mut fixture, "a");
    fixture.no_commands();
    fixture
        .update(|window, _| assert_eq!(window.find(("spotify-track", 15usize)).bounds(), before));
    open(&mut fixture, "c");
    answer(&mut fixture);
    open(&mut fixture, "b");
    answer(&mut fixture); // oldest inactive page was evicted
    fixture.no_commands();
}

#[test]
fn recent_album_pages_restore_scroll_without_refetching() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    let open = |fixture: &mut Fixture, id: &str| {
        fixture.update(|_, cx| {
            workspace.upgrade().unwrap().update(cx, |workspace, cx| {
                workspace.open_album(
                    model::AlbumRef {
                        name: id.to_owned().into(),
                        source_id: Some(id.into()),
                        ..Default::default()
                    },
                    Route::LikedSongs,
                    cx,
                );
            })
        });
    };
    let answer = |fixture: &mut Fixture, id: &str| {
        let BackendCommand::LoadAlbum { respond, .. } =
            fixture.backend.commands.try_recv().unwrap()
        else {
            panic!("album request")
        };
        respond
            .send(Ok((
                model::Album {
                    provider: model::Provider::Spotify,
                    source_id: id.into(),
                    spotify_uri: None,
                    name: id.to_owned().into(),
                    artists: vec![],
                    release_date: None,
                    artwork_url: None,
                    track_count: Some(40),
                },
                (0..40).map(track).collect(),
            )))
            .unwrap();
        fixture.update(|_, _| {});
    };
    open(&mut fixture, "a");
    answer(&mut fixture, "a");
    fixture.update(|window, cx| {
        let position = window.find(("spotify-track", 1usize)).bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-850.))),
                modifiers: Default::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    let before = fixture.update(|window, _| window.find(("spotify-track", 15usize)).bounds());
    open(&mut fixture, "b");
    answer(&mut fixture, "b");
    open(&mut fixture, "a");
    fixture.no_commands();
    fixture
        .update(|window, _| assert_eq!(window.find(("spotify-track", 15usize)).bounds(), before));
}

#[test]
fn playlist_play_skips_unavailable_start_pages_without_loading_the_whole_list() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        workspace.upgrade().unwrap().update(cx, |workspace, cx| {
            workspace.load_playlist(paged_playlist(), cx);
            workspace.navigate(Route::Playlist, cx);
        })
    });
    let BackendCommand::LoadPlaylist { respond, .. } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    respond
        .send(Ok(model::PlaylistTrackPage {
            source_offsets: vec![],
            tracks: vec![],
            next_offset: Some(15),
            total: 1000,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    fixture.no_commands();
    fixture.update(|window, cx| window.click("playlist-play", cx));
    let BackendCommand::LoadPlaylist {
        offset, respond, ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(offset, 15);
    respond
        .send(Ok(model::PlaylistTrackPage {
            source_offsets: vec![17],
            tracks: vec![track(2)],
            next_offset: Some(30),
            total: 1000,
        }))
        .unwrap();
    fixture.update(|_, _| {});
    let BackendCommand::PlayPlaylistContext {
        tracks,
        index,
        next_offset,
        ..
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!((tracks, index, next_offset), (vec![track(2)], 0, Some(30)));
    fixture.no_commands();
}

#[test]
fn discography_cards_fit_the_available_width_at_narrow_and_wide_sizes() {
    for (width, zoom, collapsed) in [(900., 125, false), (1920., 100, false), (1280., 100, true)] {
        let mut fixture = Fixture::at_width(width);
        let workspace = fixture.workspace.clone().unwrap();
        fixture.update(|_, cx| {
            cx.set_reduce_motion(true);
            appearance::Appearance::set_ui_zoom(
                crate::storage::UiZoom::from_percent(zoom).unwrap(),
                cx,
            )
            .unwrap();
            workspace.upgrade().unwrap().update(cx, |workspace, cx| {
                workspace
                    .sidebar
                    .update(cx, |sidebar, cx| sidebar.set_collapsed(collapsed, cx));
                workspace.open_artist(
                    model::ArtistRef {
                        name: "Artist".into(),
                        source_id: Some("artist".into()),
                        spotify_uri: None,
                    },
                    Route::LikedSongs,
                    cx,
                )
            });
        });
        let BackendCommand::LoadArtist { respond, .. } =
            fixture.backend.commands.try_recv().unwrap()
        else {
            panic!()
        };
        respond
            .send(Ok((
                model::Artist {
                    provider: model::Provider::Spotify,
                    name: "Artist".into(),
                    source_id: "artist".into(),
                    spotify_uri: None,
                    artwork_url: None,
                },
                (0..3).map(track).collect(),
                (0..20)
                    .map(|i| model::Album {
                        provider: model::Provider::Spotify,
                        source_id: format!("album-{i}"),
                        name: format!("Album {i}").into(),
                        spotify_uri: None,
                        artwork_url: None,
                        artists: vec![],
                        release_date: None,
                        track_count: None,
                    })
                    .collect(),
            )))
            .unwrap();
        fixture.update(|_, _| {});
        fixture.cx.advance_clock(Duration::from_secs(1));
        fixture.update(|_, _| {});
        fixture.update(|window, cx| {
            let position = window.find(("spotify-track", 1usize)).bounds().center();
            window.dispatch_event(
                gpui_kit::ScrollWheelEvent {
                    position,
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-600.))),
                    modifiers: Default::default(),
                    touch_phase: gpui_kit::TouchPhase::Moved,
                }
                .to_platform_input(),
                cx,
            );
        });
        fixture.update(|window, _| {
            let portrait = window.find("artist-portrait").bounds();
            assert!(
                portrait.left() >= window.find("search-field").bounds().right(),
                "dock must not overlap search at any zoom or sidebar width"
            );
            assert!(
                window.find("artist-moving-name").bounds().right()
                    <= window.find("account").bounds().left()
            );
            let mut found = 0;
            for index in 0usize..20 {
                if let Some(card) = window.try_find(("artist-album", index)) {
                    assert!(
                        card.bounds().right()
                            <= window.viewport_size().width
                                - gpui_kit::Rems(2.).to_pixels(window.rem_size())
                    );
                    found += 1;
                }
            }
            assert!(found >= 2);
            if width > 1800. || collapsed {
                let first = window.find(("artist-album", 0usize)).bounds();
                let fifth = window
                    .find(("artist-album", if collapsed { 5usize } else { 4usize }))
                    .bounds();
                assert_eq!(
                    first.origin.y, fifth.origin.y,
                    "wide layouts must exceed four columns"
                );
            }
        });
        fixture.no_commands();
    }
}

#[test]
fn spotify_likes_confirm_success_keep_favorites_separate_and_bound_pending_work() {
    let mut fixture = Fixture::new(false);
    fixture.right_click_track(1);
    fixture.update(|window, cx| window.click(("track-menu-spotify-like", 1usize), cx));
    let BackendCommand::SetSpotifyLiked {
        track: sent,
        liked,
        respond,
    } = fixture.backend.commands.try_recv().unwrap()
    else {
        panic!("Spotify write")
    };
    assert_eq!(sent, track(1));
    assert!(!liked);
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            assert!(
                library.is_spotify_liked(&track(1)),
                "retain confirmed state while pending"
            );
            library.set_spotify_liked(track(2), false, cx);
            library.revalidate(cx);
        });
    });
    fixture.no_commands();
    respond
        .send(Err(anyhow::anyhow!(
            "Sign in again to grant library editing permission"
        )))
        .unwrap();
    fixture.update(|_, cx| {
        assert!(
            services::AppServices::library(cx)
                .read(cx)
                .is_spotify_liked(&track(1))
        )
    });
    let workspace = fixture.workspace.clone().unwrap();
    fixture.update(|_, cx| {
        assert!(
            workspace
                .upgrade()
                .unwrap()
                .read(cx)
                .action_notice
                .is_some()
        )
    });
    fixture.right_click_track(1);
    fixture.update(|window, cx| window.click(("track-menu-spotify-like", 1usize), cx));
    let BackendCommand::SetSpotifyLiked { respond, .. } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    respond.send(Ok(())).unwrap();
    fixture.update(|_, _| {});
    fixture.update(|_, cx| {
        let library = services::AppServices::library(cx);
        let library = library.read(cx);
        assert!(!library.is_spotify_liked(&track(1)));
        assert!(library.is_favorite(&track(0)));
        assert!(!library.is_favorite(&track(1)));
    });
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.set_spotify_liked(track(1), true, cx)
        })
    });
    let BackendCommand::SetSpotifyLiked { liked, respond, .. } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    assert!(liked);
    respond.send(Ok(())).unwrap();
    fixture.update(|_, _| {});
    fixture.update(|_, cx| {
        let library = services::AppServices::library(cx);
        assert_eq!(library.read(cx).liked_tracks()[0], track(1));
        assert_eq!(
            library
                .read(cx)
                .liked_tracks()
                .iter()
                .filter(|t| **t == track(1))
                .count(),
            1
        );
    });
    fixture.no_commands();
}

#[test]
fn signing_out_discards_a_pending_spotify_like_reply() {
    let mut fixture = Fixture::new(false);
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.set_spotify_liked(track(9), true, cx)
        })
    });
    let BackendCommand::SetSpotifyLiked { respond, .. } =
        fixture.backend.commands.try_recv().unwrap()
    else {
        panic!()
    };
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| library.clear(cx))
    });
    let _ = respond.send(Ok(()));
    fixture.update(|_, cx| {
        assert!(
            services::AppServices::library(cx)
                .read(cx)
                .liked_tracks()
                .is_empty()
        )
    });
    fixture.no_commands();
}
