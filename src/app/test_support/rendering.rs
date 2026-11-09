use super::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Scene {
    Artist,
    ArtistLoading,
    ArtistDiscography,
    TrackHover,
    TrackMenu,
    AlbumTint,
    PlaylistTail,
    ReadmeGlaive,
    ReadmeGlaiveDocked,
    PlaylistsLoading,
    PlaylistTracks,
    PlaylistHover,
    PlaylistCollapsed,
    UpdateAvailable,
    PlaylistZoomSmall,
    PlaylistZoomLarge,
    ArtistMenu,
    ArtistPortrait,
    ArtistLandscape,
    ArtistZoomLarge,
    Settings,
    SettingsAutoplayOff,
    SettingsMenu,
    SettingsCache,
    Setup,
    SetupFilled,
    SearchEntered,
    SearchArtists,
    ShuffleOn,
    PlaylistTooltip,
    Queue {
        open: bool,
        hover: bool,
        pressed: bool,
    },
    QueueFocus {
        open: bool,
    },
}

fn scenes() -> Vec<(&'static str, Scene)> {
    vec![
        ("artist", Scene::Artist),
        ("artist-docked", Scene::ArtistDiscography),
        ("artist-loading", Scene::ArtistLoading),
        ("track-hover", Scene::TrackHover),
        ("track-menu", Scene::TrackMenu),
        ("album-tint", Scene::AlbumTint),
        ("playlist-tail", Scene::PlaylistTail),
        ("artist-zoom-large", Scene::ArtistZoomLarge),
        ("artist-account-menu", Scene::ArtistMenu),
        ("playlists-loading", Scene::PlaylistsLoading),
        ("playlist-tracks", Scene::PlaylistTracks),
        ("sidebar-playlist-hover", Scene::PlaylistHover),
        ("sidebar-collapsed", Scene::PlaylistCollapsed),
        ("update-available", Scene::UpdateAvailable),
        ("playlist-zoom-small", Scene::PlaylistZoomSmall),
        ("playlist-zoom-large", Scene::PlaylistZoomLarge),
        ("settings", Scene::Settings),
        ("settings-cache", Scene::SettingsCache),
        ("settings-autoplay-off", Scene::SettingsAutoplayOff),
        ("settings-mascot-open", Scene::SettingsMenu),
        ("setup", Scene::Setup),
        ("setup-filled", Scene::SetupFilled),
        ("search-entered", Scene::SearchEntered),
        ("search-artists", Scene::SearchArtists),
        ("shuffle-on", Scene::ShuffleOn),
        ("sidebar-tooltip", Scene::PlaylistTooltip),
        (
            "queue-closed",
            Scene::Queue {
                open: false,
                hover: false,
                pressed: false,
            },
        ),
        (
            "queue-hover",
            Scene::Queue {
                open: false,
                hover: true,
                pressed: false,
            },
        ),
        (
            "queue-pressed",
            Scene::Queue {
                open: false,
                hover: true,
                pressed: true,
            },
        ),
        (
            "queue-open",
            Scene::Queue {
                open: true,
                hover: false,
                pressed: false,
            },
        ),
        (
            "queue-open-hover",
            Scene::Queue {
                open: true,
                hover: true,
                pressed: false,
            },
        ),
        (
            "queue-open-pressed",
            Scene::Queue {
                open: true,
                hover: true,
                pressed: true,
            },
        ),
        ("queue-focus", Scene::QueueFocus { open: false }),
        ("queue-open-focus", Scene::QueueFocus { open: true }),
    ]
}

fn capture(scene: Scene, theme: ThemePreference, width: f32) -> image::RgbaImage {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(assets::CadenceAssets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        fonts::register(cx).expect("bundled Inter fonts");
    });
    let mut backend = cx.update(|cx| initialize(cx, theme));
    let height = if matches!(scene, Scene::ReadmeGlaiveDocked) {
        640.
    } else if matches!(scene, Scene::ReadmeGlaive) {
        960.
    } else {
        820.
    };
    let handle = if matches!(scene, Scene::Setup | Scene::SetupFilled) {
        cx.update(|cx| {
            services::AppServices::session(cx).update(cx, |session, cx| {
                session.handle_backend_event(BackendEvent::SetupRequired, cx);
            });
        });
        cx.open_window(size(px(width), px(height)), |window, cx| {
            appearance::Appearance::attach(window, cx);
            let view = cx.new(|cx| onboarding::Onboarding::new(window, cx));
            if matches!(scene, Scene::SetupFilled) {
                view.update(cx, |view, cx| view.focus_setup_field(window, cx));
            }
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("setup window")
    } else {
        let (handle, workspace) = workspace(&mut cx, width, height);
        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| match scene {
                Scene::Artist | Scene::ArtistDiscography
                | Scene::ArtistMenu
                | Scene::ArtistPortrait
                | Scene::ArtistLandscape | Scene::ArtistZoomLarge => {
                    if matches!(scene, Scene::ArtistZoomLarge) { appearance::Appearance::set_ui_zoom(crate::storage::UiZoom::from_percent(125).unwrap(), cx).unwrap(); }
                    let (image_width, image_height) = match scene {
                        Scene::ArtistPortrait => (360, 1080),
                        Scene::ArtistLandscape => (1080, 360),
                        _ => (640, 640),
                    };
                    let url = "https://fixtures.invalid/artist.png";
                    let pixels = image::RgbaImage::from_fn(image_width, image_height, |x, y| {
                        // A local, deterministic artwork fixture; no network or
                        // copyrighted artist image is needed for layout checks.
                        let stripe = if (x / 70 + y / 110) % 2 == 0 { 20 } else { 0 };
                        image::Rgba([
                            80 + (x * 120 / image_width) as u8,
                            65 + (y * 120 / image_height) as u8,
                            140 + stripe,
                            255,
                        ])
                    });
                    services::AppServices::image_cache(cx).update(cx, |cache, _| {
                        cache.seed_artist_fixture(
                            url,
                            Arc::new(gpui_kit::RenderImage::new([image::Frame::new(pixels)])),
                        );
                    });
                    let reference = model::ArtistRef {
                        name: "The Midnight".into(),
                        source_id: Some("fixture-artist".into()),
                        spotify_uri: None,
                    };
                    workspace.open_artist(reference, Route::LikedSongs, cx);
                    match backend.commands.try_recv().expect("artist request") {
                        BackendCommand::LoadArtist { respond, .. } => {
                            respond
                                .send(Ok((
                                    model::Artist {
                                        provider: model::Provider::Spotify,
                                        source_id: "fixture-artist".into(),
                                        spotify_uri: None,
                                        name: "The Midnight".into(),
                                        artwork_url: Some(url.into()),
                                    },
                                    vec![track(0), track(1), track(2)],
                                    (0..20)
                                        .map(|index| model::Album {
                                            provider: model::Provider::Spotify,
                                            source_id: format!("fixture-album-{index}"),
                                            spotify_uri: None,
                                            name: format!("Release {}", index + 1).into(),
                                            artists: Vec::new(),
                                            release_date: Some("2026-06-01".into()),
                                            artwork_url: Some(url.into()),
                                            track_count: Some(10),
                                        })
                                        .collect(),
                                )))
                                .expect("artist fixture reply");
                        }
                        command => panic!("unexpected artist command: {command:?}"),
                    }
                }
                Scene::ArtistLoading => workspace.open_artist(model::ArtistRef { name: "Artist".into(), source_id: Some("loading".into()), spotify_uri: None }, Route::LikedSongs, cx),
                Scene::AlbumTint => {
                    let url = "https://fixtures.invalid/blue-album.png";
                    let pixels = image::RgbaImage::from_pixel(64, 64, image::Rgba([210, 110, 70, 255]));
                    services::AppServices::image_cache(cx).update(cx, |cache, _| cache.seed_artist_fixture(url, Arc::new(gpui_kit::RenderImage::new([image::Frame::new(pixels)]))));
                    workspace.open_album(model::AlbumRef { name: "Blue hours".into(), source_id: Some("blue-album".into()), artwork_url: Some(url.into()), ..Default::default() }, Route::LikedSongs, cx);
                    let BackendCommand::LoadAlbum { respond, .. } = backend.commands.try_recv().unwrap() else { panic!() };
                    respond.send(Ok((model::Album { provider: model::Provider::Spotify, source_id: "blue-album".into(), spotify_uri: None, name: "Blue hours".into(), artists: vec![], release_date: Some("2026-01-01".into()), artwork_url: Some(url.into()), track_count: Some(30) }, (0..30).map(track).collect()))).unwrap();
                }
                Scene::ReadmeGlaive | Scene::ReadmeGlaiveDocked => {
                    let path = std::env::var("CADENCE_README_FIXTURE").expect("README fixture path");
                    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                    let artist: model::Artist = serde_json::from_value(value["artist"].clone()).unwrap();
                    let tracks: Vec<model::Track> = serde_json::from_value(value["tracks"].clone()).unwrap();
                    let albums: Vec<model::Album> = serde_json::from_value(value["albums"].clone()).unwrap();
                    for artwork in value["artworks"].as_array().unwrap() {
                        let path = artwork["path"].as_str().unwrap();
                        let mut pixels = image::open(path).unwrap().to_rgba8();
                        for pixel in pixels.pixels_mut() { pixel.0.swap(0, 2); }
                        services::AppServices::image_cache(cx).update(cx, |cache, _| cache.seed_artist_fixture(artwork["url"].as_str().unwrap(), Arc::new(gpui_kit::RenderImage::new([image::Frame::new(pixels)]))));
                    }
                    workspace.open_artist(model::ArtistRef { name: artist.name.clone(), source_id: Some(artist.source_id.clone()), spotify_uri: artist.spotify_uri.clone() }, Route::LikedSongs, cx);
                    let BackendCommand::LoadArtist { respond, .. } = backend.commands.try_recv().unwrap() else { panic!() };
                    respond.send(Ok((artist, tracks.clone(), albums))).unwrap();
                    services::AppServices::player(cx).update(cx, |player, cx| { player.handle_backend_event(BackendEvent::PlaybackSnapshotLoaded { current: tracks[0].clone(), next: tracks[1..].to_vec(), position_ms: 60_000 }, cx); });
                }
                Scene::PlaylistsLoading => {
                    workspace.library.update(cx, |library, cx| library.clear(cx));
                    workspace.navigate(Route::Playlists, cx);
                }
                Scene::PlaylistTracks | Scene::PlaylistHover | Scene::PlaylistZoomSmall | Scene::PlaylistZoomLarge | Scene::PlaylistCollapsed | Scene::PlaylistTooltip | Scene::PlaylistTail => {
                    let artwork_url = "https://fixtures.invalid/sidebar-cover.png";
                    let cover = image::RgbaImage::from_fn(64, 64, |x, y| image::Rgba([45 + (x*2) as u8, 50 + (y*2) as u8, 160, 255]));
                    services::AppServices::image_cache(cx).update(cx, |cache, _| cache.seed_artist_fixture(artwork_url, Arc::new(gpui_kit::RenderImage::new([image::Frame::new(cover)]))));
                    if matches!(scene, Scene::PlaylistZoomSmall | Scene::PlaylistZoomLarge) {
                        let percent = if matches!(scene, Scene::PlaylistZoomSmall) { 75 } else { 125 };
                        appearance::Appearance::set_ui_zoom(crate::storage::UiZoom::from_percent(percent).unwrap(), cx).unwrap();
                    }
                    let playlists: Vec<_> = (0..200).map(|index| model::Playlist {
                        provider: model::Provider::Spotify, source_id: format!("fixture-playlist-{index}"),
                        name: if index == 0 { "A playlist with an especially long title that must stay within the sidebar".into() } else { format!("Playlist {}", index + 1).into() },
                        owner: "Cadence Ensemble".into(), track_count: 1000, artwork_url: (index % 2 == 0).then(|| artwork_url.into()),
                    }).collect();
                    let selected = playlists[0].clone();
                    workspace.library.update(cx, |library, cx| { library.handle_backend_event(BackendEvent::LibraryLoaded { generation: 0, liked_tracks: vec![track(0)], playlists }, 0, cx); });
                    if matches!(scene, Scene::PlaylistCollapsed | Scene::PlaylistTooltip) { workspace.sidebar.update(cx, |sidebar, cx| sidebar.set_collapsed(true, cx)); }
                    workspace.load_playlist(selected, cx);
                    workspace.navigate(Route::Playlist, cx);
                    match backend.commands.try_recv().expect("playlist request") {
                        BackendCommand::LoadPlaylist { respond, .. } => { respond.send(Ok(model::PlaylistTrackPage { source_offsets: vec![], tracks: (0..15).map(track).collect(), next_offset: Some(15), total: 1000 })).unwrap(); }
                        _ => panic!("expected playlist request"),
                    }
                }
                Scene::SearchArtists => {
                    workspace.search.update(cx, |search, cx| {
                        search.set_query("glaive".into(), cx);
                        search.submit(cx);
                    });
                    workspace.navigate(Route::Search, cx);
                    let BackendCommand::SearchCatalog { respond, .. } = backend.commands.try_recv().unwrap() else { panic!("search request") };
                    respond.send(Ok(model::CatalogSearchResults {
                        artists: ["glaive", "glaive radio", "glaive ensemble"].into_iter().enumerate().map(|(index, name)| model::Artist {
                            provider: model::Provider::Spotify, source_id: format!("artist-{index}"), spotify_uri: None, name: name.into(), artwork_url: None,
                        }).collect(), tracks: (0..10).map(track).collect(), playlists: vec![],
                    })).unwrap();
                }
                Scene::ShuffleOn => { services::AppServices::player(cx).update(cx, |player, cx| player.toggle_shuffle(cx)); }
                Scene::UpdateAvailable => { workspace.toolbar.update(cx, |toolbar, cx| toolbar.seed_update_available(cx)); }
                Scene::Settings | Scene::SettingsAutoplayOff | Scene::SettingsMenu | Scene::SettingsCache => {
                    workspace.open_settings(cx);
                    if matches!(scene, Scene::SettingsAutoplayOff) {
                        services::AppServices::set_autoplay(false, cx)
                            .expect("settings store")
                            .expect("save autoplay");
                        workspace.settings.update(cx, |_, cx| cx.notify());
                    }
                }
                Scene::Queue { open, .. } => {
                    workspace
                        .player_bar
                        .update(cx, |bar, cx| bar.set_queue_open(open, cx));
                    cx.notify();
                }
                _ => {}
            });
        });
        handle
    };
    cx.update_window(handle.into(), |_, window, _| window.activate_window())
        .expect("activate fixture window");
    settle(&mut cx, handle.into());
    match scene {
        Scene::SettingsCache => {
            cx.update_window(handle.into(), |_, window, cx| {
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
            })
            .unwrap();
            settle(&mut cx, handle.into());
        }
        Scene::ArtistDiscography | Scene::ReadmeGlaiveDocked => {
            cx.update_window(handle.into(), |_, window, cx| {
                let position = window.find(("spotify-track", 0usize)).bounds().center();
                window.dispatch_event(
                    gpui_kit::ScrollWheelEvent {
                        position,
                        delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-450.))),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .unwrap();
            settle(&mut cx, handle.into());
            cx.update_window(handle.into(), |_, window, cx| window.hover("account", cx))
                .unwrap();
            settle(&mut cx, handle.into());
        }
        Scene::TrackMenu => {
            cx.update_window(handle.into(), |_, window, cx| {
                let position = window.find(("spotify-track", 1usize)).bounds().center();
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
            })
            .unwrap();
            settle(&mut cx, handle.into());
        }
        Scene::TrackHover => {
            cx.update_window(handle.into(), |_, window, cx| {
                window.hover(("spotify-track", 1usize), cx)
            })
            .unwrap();
            settle(&mut cx, handle.into());
        }
        Scene::PlaylistTail => {
            cx.update_window(handle.into(), |_, window, cx| {
                let position = window.find(("spotify-track", 1usize)).bounds().center();
                window.dispatch_event(
                    gpui_kit::ScrollWheelEvent {
                        position,
                        delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                        modifiers: Default::default(),
                        touch_phase: gpui_kit::TouchPhase::Moved,
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .unwrap();
            settle(&mut cx, handle.into());
        }
        Scene::PlaylistTooltip => {
            cx.update_window(handle.into(), |_, window, cx| {
                window.hover(("sidebar-playlist", 0usize), cx)
            })
            .unwrap();
            settle(&mut cx, handle.into());
        }
        Scene::PlaylistHover => {
            cx.update_window(handle.into(), |_, window, cx| {
                window.hover(("sidebar-playlist", 0usize), cx)
            })
            .unwrap();
            settle(&mut cx, handle.into());
        }
        Scene::ArtistMenu => {
            cx.update_window(handle.into(), |_, window, cx| {
                let position = window.find("account").bounds().center();
                window.dispatch_event(
                    MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }
                    .to_platform_input(),
                    cx,
                );
                window.dispatch_event(
                    MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .expect("open artist account menu");
            settle(&mut cx, handle.into());
            cx.update_window(handle.into(), |_, window, _| {
                assert!(window.find("account-settings").bounds().size.height > px(0.));
            })
            .expect("artist menu is visible over backdrop");
        }
        Scene::SearchEntered => {
            press_key(
                &mut cx,
                handle.into(),
                if cfg!(target_os = "macos") {
                    "cmd-k"
                } else {
                    "ctrl-k"
                },
            );
            for character in "Blue Train".chars() {
                press_key(&mut cx, handle.into(), &character.to_string());
            }
        }
        Scene::QueueFocus { open } => {
            press_key(
                &mut cx,
                handle.into(),
                if cfg!(target_os = "macos") {
                    "cmd-k"
                } else {
                    "ctrl-k"
                },
            );
            tab_to(&mut cx, handle.into(), "queue-toggle");
            if open {
                press_key(&mut cx, handle.into(), "space");
            }
        }
        Scene::SetupFilled => {
            for character in "0123456789abcdef0123456789abcdef".chars() {
                press_key(&mut cx, handle.into(), &character.to_string());
            }
        }
        Scene::SettingsMenu => {
            cx.update_window(handle.into(), |_, window, cx| {
                window.within("settings-mascot").click("input", cx);
            })
            .expect("open mascot selector");
            settle(&mut cx, handle.into());
            cx.update_window(handle.into(), |_, window, _| {
                assert_eq!(window.find("settings-mascot").expanded(), Some(true));
            })
            .expect("mascot menu is actually open");
        }
        _ => {}
    }
    if let Scene::Queue {
        hover: true,
        pressed,
        ..
    } = scene
    {
        let compact = uses_compact_player_layout(px(width), px(16.));
        let controls_width = if compact { 88. } else { 216. };
        let position = point(px(width - 24. - controls_width + 20.), px(height - 48.));
        cx.update_window(handle.into(), |_, window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }
                .to_platform_input(),
                cx,
            );
            if pressed {
                window.dispatch_event(
                    MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }
                    .to_platform_input(),
                    cx,
                );
            }
        })
        .expect("queue pointer state");
        settle(&mut cx, handle.into());
    }
    if matches!(
        scene,
        Scene::Artist
            | Scene::ArtistMenu
            | Scene::ArtistPortrait
            | Scene::ArtistLandscape
            | Scene::ArtistZoomLarge
    ) {
        cx.update_window(handle.into(), |_, window, _| {
            let bounds = window.find("artist-portrait").bounds();
            assert_eq!(bounds.size.width, Rems(10.).to_pixels(window.rem_size()));
            assert_eq!(bounds.size.height, Rems(10.).to_pixels(window.rem_size()));
            let popular = window.find("artist-popular-heading").bounds();
            let last_track = window.find(("spotify-track", 2usize)).bounds();
            assert!(popular.origin.y < last_track.origin.y);
            if let Some(discography) = window.try_find("artist-discography-heading") {
                let discography = discography.bounds();
                assert!(discography.origin.y >= last_track.bottom());
                assert!(
                    discography.origin.y - last_track.bottom()
                        <= Rems(2.).to_pixels(window.rem_size())
                );
            } else {
                // At increased zoom the taller header pushes this virtualized
                // section below the viewport; scrolling is tested separately.
                assert!(
                    last_track.bottom()
                        >= window.viewport_size().height - Rems(6.).to_pixels(window.rem_size())
                );
            }
            let backdrop = window.find("artist-backdrop-image").bounds();
            let header = window.find("artist-header").bounds();
            assert_eq!(
                backdrop.size, header.size,
                "backdrop must cover the full header for every source aspect ratio"
            );
        })
        .expect("square artist portrait");
    }
    let screenshot = cx
        .capture_screenshot(handle.into())
        .expect("native Metal screenshot");
    if matches!(
        scene,
        Scene::Artist
            | Scene::ArtistPortrait
            | Scene::ArtistLandscape
            | Scene::ArtistZoomLarge
            | Scene::ReadmeGlaive
    ) {
        let seam = cx
            .update_window(handle.into(), |_, window, _| {
                window
                    .find("artist-header")
                    .bounds()
                    .to_device_pixels(window.scale_factor())
            })
            .unwrap();
        let bottom: i32 = seam.bottom().into();
        let left: i32 = seam.left().into();
        let right: i32 = seam.right().into();
        for x in (left + 8..right - 8).step_by(31) {
            assert_eq!(
                screenshot.get_pixel(x as u32, (bottom - 8) as u32),
                screenshot.get_pixel(x as u32, (bottom + 1) as u32),
                "artist backdrop must be solid page colour before the bottom edge"
            );
        }
    }
    if let Scene::QueueFocus { open } = scene {
        let bounds = cx
            .update_window(handle.into(), |_, window, _| {
                let button = window.find("queue-toggle");
                assert_eq!(button.focused(), Some(true), "queue keyboard focus");
                assert_eq!(button.expanded(), Some(open), "queue expansion");
                button
                    .bounds()
                    .dilate(px(4.))
                    .to_device_pixels(window.scale_factor())
            })
            .expect("queue focus bounds");
        image::imageops::crop_imm(
            &screenshot,
            bounds.origin.x.into(),
            bounds.origin.y.into(),
            bounds.size.width.into(),
            bounds.size.height.into(),
        )
        .to_image()
    } else {
        screenshot
    }
}

fn save_artifact(image: &image::RgbaImage, path: &Path) {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};

    let file = std::fs::File::create(path).expect("create visual artifact");
    PngEncoder::new_with_quality(file, CompressionType::Best, FilterType::Adaptive)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .expect("write visual artifact");
}

fn compare(
    name: &str,
    actual: &image::RgbaImage,
    expected: &image::RgbaImage,
    artifacts: &Path,
) -> bool {
    if expected == actual {
        return true;
    }
    std::fs::create_dir_all(artifacts).expect("create visual artifacts directory");
    save_artifact(actual, &artifacts.join(format!("{name}-actual.png")));
    save_artifact(expected, &artifacts.join(format!("{name}-expected.png")));
    let difference = image::RgbaImage::from_fn(
        actual.width().max(expected.width()),
        actual.height().max(expected.height()),
        |x, y| {
            if actual.get_pixel_checked(x, y) == expected.get_pixel_checked(x, y) {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 0, 128, 255])
            }
        },
    );
    save_artifact(&difference, &artifacts.join(format!("{name}-diff.png")));
    eprintln!("pixel mismatch: {name}");
    false
}

pub(crate) fn run() {
    let baseline = std::env::var_os("CADENCE_UI_BASELINE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/visual/baseline"));
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-artifacts");
    if std::env::args().any(|argument| argument == "--artist-scroll-preview") {
        let previews = artifacts.with_file_name("artist-scroll-previews");
        std::fs::create_dir_all(&previews).unwrap();
        for (name, scene) in [
            ("expanded", Scene::ReadmeGlaive),
            ("docked", Scene::ReadmeGlaiveDocked),
        ] {
            let image = capture(scene, ThemePreference::Dark, 1280.);
            let repeated = capture(scene, ThemePreference::Dark, 1280.);
            assert!(compare(name, &image, &repeated, &previews));
            save_artifact(&image, &previews.join(format!("{name}.png")));
        }
        return;
    }
    if std::env::args().any(|argument| argument == "--detail-layout-preview") {
        let previews = artifacts.with_file_name("detail-layout-previews");
        std::fs::create_dir_all(&previews).expect("preview directory");
        for (name, scene, width, theme) in [
            (
                "glaive-profile",
                Scene::ReadmeGlaive,
                1280.,
                ThemePreference::Dark,
            ),
            (
                "artist-wide",
                Scene::ArtistDiscography,
                1920.,
                ThemePreference::Dark,
            ),
            ("artist-compact", Scene::Artist, 900., ThemePreference::Dark),
            ("album", Scene::AlbumTint, 1280., ThemePreference::Dark),
            (
                "playlist",
                Scene::PlaylistTracks,
                1280.,
                ThemePreference::Dark,
            ),
            ("artist-light", Scene::Artist, 1280., ThemePreference::Light),
        ] {
            let image = capture(scene, theme, width);
            let repeated = capture(scene, theme, width);
            assert!(
                compare(name, &image, &repeated, &previews),
                "preview must repeat exactly"
            );
            let path = previews.join(format!("{name}.png"));
            save_artifact(&image, &path);
            println!("{}", path.display());
        }
        return;
    }
    if std::env::args().any(|argument| argument == "--readme-glaive") {
        let screenshot = capture(Scene::ReadmeGlaive, ThemePreference::Dark, 1280.);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/glaive-profile.png");
        save_artifact(&screenshot, &path);
        println!("{}", path.display());
        return;
    }
    if std::env::args().any(|argument| argument == "--artist-overlay-preview") {
        let previews = artifacts.with_file_name("artist-previews");
        std::fs::create_dir_all(&previews).expect("preview directory");
        for (layout, width) in [("regular", 1280.), ("compact", 900.)] {
            let image = capture(Scene::Artist, ThemePreference::Dark, width);
            let repeated = capture(Scene::Artist, ThemePreference::Dark, width);
            assert!(
                compare("artist-overlay-repeat", &image, &repeated, &previews),
                "overlay preview must repeat exactly"
            );
            let path = previews.join(format!("artist-overlay-{layout}-preview.png"));
            save_artifact(&image, &path);
            println!("{}", path.display());
            let menu = capture(Scene::ArtistMenu, ThemePreference::Dark, width);
            let repeated = capture(Scene::ArtistMenu, ThemePreference::Dark, width);
            assert!(compare("artist-menu-repeat", &menu, &repeated, &previews));
            let path = previews.join(format!("artist-glass-menu-{layout}-preview.png"));
            save_artifact(&menu, &path);
            println!("{}", path.display());
        }
        return;
    }
    if std::env::args().any(|argument| argument == "--artist-preview") {
        let previews = artifacts.with_file_name("artist-previews");
        std::fs::create_dir_all(&previews).expect("preview directory");
        for (theme_name, theme) in [
            ("light", ThemePreference::Light),
            ("dark", ThemePreference::Dark),
        ] {
            for (layout, width) in [("regular", 1280.), ("compact", 900.)] {
                for (aspect, scene) in [
                    ("square", Scene::Artist),
                    ("portrait", Scene::ArtistPortrait),
                    ("landscape", Scene::ArtistLandscape),
                ] {
                    let image = capture(scene, theme, width);
                    let repeated = capture(scene, theme, width);
                    assert!(
                        compare("artist-repeat", &image, &repeated, &previews),
                        "artist rendering must repeat exactly"
                    );
                    let path =
                        previews.join(format!("artist-{theme_name}-{layout}-{aspect}-preview.png"));
                    save_artifact(&image, &path);
                    println!("{}", path.display());
                }
            }
        }
        return;
    }
    if artifacts.exists() {
        std::fs::remove_dir_all(&artifacts).expect("clear previous visual artifacts");
    }
    let record = std::env::args().any(|argument| argument == "--record");
    if record {
        assert!(
            std::env::var_os("CADENCE_UI_BASELINE").is_some(),
            "Recording requires an explicit CADENCE_UI_BASELINE directory"
        );
        std::fs::create_dir_all(&baseline).expect("create baseline directory");
    }
    let mut mismatches = 0;
    let mut count = 0;
    for (theme_name, theme) in [
        ("light", ThemePreference::Light),
        ("dark", ThemePreference::Dark),
    ] {
        for (layout, width) in [("regular", 1280.), ("compact", 900.)] {
            for (name, scene) in scenes() {
                let name = format!("{theme_name}-{layout}-{name}");
                let actual = capture(scene, theme, width);
                let repeated = capture(scene, theme, width);
                assert!(
                    compare(&format!("{name}-repeat"), &repeated, &actual, &artifacts),
                    "nonrepeatable rendering: {name}"
                );
                if record {
                    actual
                        .save(baseline.join(format!("{name}.png")))
                        .expect("record baseline");
                } else {
                    let expected_path = baseline.join(format!("{name}.png"));
                    match image::open(&expected_path) {
                        Ok(expected) => {
                            if !compare(&name, &actual, &expected.to_rgba8(), &artifacts) {
                                mismatches += 1;
                            }
                        }
                        Err(error) => {
                            std::fs::create_dir_all(&artifacts)
                                .expect("create artifacts directory");
                            save_artifact(&actual, &artifacts.join(format!("{name}-actual.png")));
                            eprintln!("read {}: {error}", expected_path.display());
                            mismatches += 1;
                        }
                    }
                }
                count += 1;
            }
        }
    }
    assert_eq!(
        mismatches,
        0,
        "native visual regressions, see {}",
        artifacts.display()
    );
    println!("Native visual checks: {count} repeatable cases, {mismatches} mismatches");
}
