use super::*;
use gpui_kit::TestSupportExt as _;

/// Navigation the sidebar asks the workspace to perform.
pub(super) enum SidebarEvent {
    Navigate(Route),
    Failed(SharedString),
    OpenPlaylist {
        playlist: model::Playlist,
        origin: Route,
    },
}

/// A library route the rail links to.
struct NavItem {
    id: &'static str,
    fill_id: &'static str,
    label: &'static str,
    icon: CadenceIcon,
    selected_icon: CadenceIcon,
    route: Route,
}

const NAV_ITEMS: [NavItem; 4] = [
    NavItem {
        id: "nav-home",
        fill_id: "nav-home-fill",
        label: "Home",
        icon: CadenceIcon::Home,
        selected_icon: CadenceIcon::HomeFilled,
        route: Route::Home,
    },
    NavItem {
        id: "nav-library",
        fill_id: "nav-library-fill",
        label: "Liked Songs",
        icon: CadenceIcon::Heart,
        selected_icon: CadenceIcon::HeartFilled,
        route: Route::LikedSongs,
    },
    NavItem {
        id: "nav-playlist",
        fill_id: "nav-playlist-fill",
        label: "Playlists",
        icon: CadenceIcon::Playlist,
        selected_icon: CadenceIcon::PlaylistFilled,
        route: Route::Playlists,
    },
    NavItem {
        id: "nav-recent",
        fill_id: "nav-recent-fill",
        label: "Recently played",
        icon: CadenceIcon::Clock,
        selected_icon: CadenceIcon::ClockFilled,
        route: Route::Recent,
    },
];

/// Where the rows stand in the rail's collapse or expand animation.
struct RowTransition {
    generation: usize,
    animation: Animation,
    start_progress: f32,
    target_progress: f32,
    row_width: Rems,
}

/// The library navigation rail.
pub(super) struct Sidebar {
    library: Entity<library::Library>,
    brand_mark: Arc<gpui_kit::Image>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    tooltip_anchor: Rc<Cell<Bounds<Pixels>>>,
    playlist_hover: components::HoverHighlight,
    playlist_source: Arc<[model::Playlist]>,
    playlist_order: Arc<[usize]>,
    pinned_source: Arc<[model::Playlist]>,
    pinned_count: usize,
    playlist_scroll: gpui_kit::UniformListScrollHandle,
    active_playlist: Option<String>,
    reorder_generation: usize,
    reorder_position: usize,
    reorder_destination: usize,
    reorder_started: Option<std::time::Instant>,
    /// The route to highlight, pushed by the workspace when it navigates.
    route: Route,
    /// Where a pinned playlist should return to when the listener backs out.
    pinned_origin: Route,
    compact_layout: bool,
    hover: components::HoverHighlight,
    collapsed: bool,
    transition_generation: u64,
    visual_width: Rems,
    transition_from: Rems,
    transition_duration: Duration,
    /// The toggle and navigation rows draw keyboard focus on the fill inside
    /// them, so they own their focus handles.
    toggle_focus: FocusHandle,
    /// One per entry of [`NAV_ITEMS`], in the same order.
    nav_focus: [FocusHandle; NAV_ITEMS.len()],
}

impl EventEmitter<SidebarEvent> for Sidebar {}

fn expanded_sidebar_width(compact_layout: bool) -> Rems {
    if compact_layout {
        COMPACT_EXPANDED_SIDEBAR_WIDTH
    } else {
        EXPANDED_SIDEBAR_WIDTH
    }
}

fn item_label(index: usize) -> &'static str {
    NAV_ITEMS[index].label
}

impl Sidebar {
    pub(super) fn new(collapsed: bool, cx: &mut App) -> Self {
        let width = if collapsed {
            COLLAPSED_SIDEBAR_WIDTH
        } else {
            expanded_sidebar_width(false)
        };
        Self {
            library: services::AppServices::library(cx),
            image_cache: services::AppServices::image_cache(cx),
            tooltip_anchor: Rc::new(Cell::new(Bounds::default())),
            playlist_hover: components::HoverHighlight::default(),
            playlist_source: Arc::from([]),
            playlist_order: Arc::from([]),
            pinned_source: Arc::from([]),
            pinned_count: 0,
            playlist_scroll: gpui_kit::UniformListScrollHandle::new(),
            active_playlist: None,
            reorder_generation: 0,
            reorder_position: 0,
            reorder_destination: 0,
            reorder_started: None,
            brand_mark: services::AppServices::brand_mark(cx),
            route: Route::LikedSongs,
            pinned_origin: Route::LikedSongs,
            compact_layout: false,
            hover: components::HoverHighlight::default(),
            collapsed,
            transition_generation: 0,
            visual_width: width,
            transition_from: width,
            transition_duration: Duration::from_millis(1),
            toggle_focus: cx.focus_handle().tab_stop(true),
            nav_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
        }
    }

    pub(super) fn show_route(
        &mut self,
        route: Route,
        pinned_origin: Route,
        active_playlist: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.route != route
            || self.pinned_origin != pinned_origin
            || self.active_playlist != active_playlist
        {
            if route == Route::Playlist
                && self.active_playlist != active_playlist
                && let Some(position) = self.playlist_order.iter().position(|index| {
                    Some(self.playlist_source[*index].source_id.as_str())
                        == active_playlist.as_deref()
                })
                && position > 0
            {
                self.promote_playlist(position, cx);
            }
            self.active_playlist = active_playlist;
            self.route = route;
            self.pinned_origin = pinned_origin;
            cx.notify();
        }
    }

    fn promote_playlist(&mut self, position: usize, cx: &mut Context<Self>) {
        let destination = self.pinned_count;
        // Pins retain their order and cannot be displaced by recency.
        if position <= destination {
            return;
        }
        let mut order = self.playlist_order.to_vec();
        self.reorder_position = position;
        self.reorder_destination = destination;
        self.reorder_started = Some(cx.background_executor().now());
        let index = order.remove(position);
        order.insert(destination, index);
        self.playlist_order = order.into();
        self.playlist_scroll
            .scroll_to_item(0, gpui_kit::ScrollStrategy::Top);
        self.reorder_generation = self.reorder_generation.wrapping_add(1);
        cx.notify();
    }

    pub(super) fn set_compact_layout(&mut self, compact: bool, cx: &mut Context<Self>) {
        if self.compact_layout != compact {
            self.compact_layout = compact;
            cx.notify();
        }
    }

    pub(super) fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.collapsed == collapsed {
            return;
        }
        let current_width = self.visual_width;
        let expanded_width = expanded_sidebar_width(self.compact_layout);
        let target_width = if collapsed {
            COLLAPSED_SIDEBAR_WIDTH
        } else {
            expanded_width
        };
        self.transition_from = current_width;
        self.transition_duration =
            sidebar_transition_duration(current_width, target_width, expanded_width);
        self.collapsed = collapsed;
        self.transition_generation = self.transition_generation.wrapping_add(1);
        if let Some(result) = services::AppServices::set_sidebar_collapsed(collapsed, cx)
            && let Some(error) = services::preference_save_error("sidebar", result)
        {
            cx.emit(SidebarEvent::Failed(error));
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn rendered_width(&self) -> Rems {
        self.visual_width
    }

    fn nav_item(
        &self,
        item: &NavItem,
        focus_handle: &FocusHandle,
        transition: &RowTransition,
        palette: CadencePalette,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let index = NAV_ITEMS
            .iter()
            .position(|nav| nav.id == item.id)
            .unwrap_or(0);
        let route = self.route;
        let target = item.route;
        let selected = route == target;
        let glyph = if selected {
            item.selected_icon
        } else {
            item.icon
        };
        let base = if selected {
            palette.selection
        } else {
            palette.canvas
        };
        let fill_color = self
            .hover
            .transition(index, cx.background_executor().now())
            .map_or_else(
                || rgb(base),
                |hover| hover.color(window, base, palette.control_hover, cx.reduce_motion()),
            );
        let focus_visible = components::is_focus_visible(focus_handle, window);
        let start_progress = transition.start_progress;
        let target_progress = transition.target_progress;
        let row_width = transition.row_width;
        // The pill carries selection and hover, sized to what it visually
        // covers: the icon when collapsed, the whole row when expanded.
        let fill = div()
            .h(tokens::NAV_ROW_HEIGHT)
            .rounded_xl()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap_3()
            .pr(NAV_ROW_PAD)
            .bg(fill_color)
            .when(focus_visible, |fill| {
                fill.shadow(components::inset_focus_ring(palette))
            })
            .child(
                div()
                    .w_5()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(components::icon(
                        glyph,
                        tokens::CONTROL_ICON,
                        palette.text_primary,
                    )),
            )
            .child(div().whitespace_nowrap().child(item.label).with_animation(
                (item.id, transition.generation),
                transition.animation.clone(),
                move |label, delta| {
                    label.opacity(start_progress + (target_progress - start_progress) * delta)
                },
            ))
            .with_animation(
                (item.fill_id, transition.generation),
                transition.animation.clone(),
                move |fill, delta| {
                    let progress = start_progress + (target_progress - start_progress) * delta;
                    let (width, left, pad) =
                        sidebar_fill_geometry(NAV_ROW_PAD, NAV_GLYPH_WIDTH, row_width, progress);
                    fill.w(width).ml(left).pl(pad)
                },
            );
        components::bare_button(item.id)
            .test_support()
            .track_focus(focus_handle)
            .aria_selected(selected)
            .w_full()
            .h(tokens::NAV_ROW_HEIGHT)
            .justify_start()
            .text_color(rgb(if selected {
                palette.text_primary
            } else {
                palette.text
            }))
            .text_sm()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .child(fill)
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.hover
                    .set(index, *hovered, cx.background_executor().now());
                cx.notify();
            }))
            .when(self.collapsed, |row| {
                rail_tooltip(row, item_label(index).into(), self.tooltip_anchor.clone())
            })
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::Navigate(target))))
            .into_any_element()
    }

    fn panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let collapsed = self.collapsed;
        let pinned_origin = self.pinned_origin;
        let expanded_width = expanded_sidebar_width(self.compact_layout);
        let target_width = if collapsed {
            COLLAPSED_SIDEBAR_WIDTH
        } else {
            expanded_width
        };
        let start_width = self.transition_from;
        let animation_id = self.transition_generation as usize;
        let animation_duration = self.transition_duration;
        // Reflow the page once to its final width. Only local rail labels and
        // fills animate, rather than repeatedly measuring the whole content grid.
        self.visual_width = target_width;
        let width_range = expanded_width - COLLAPSED_SIDEBAR_WIDTH;
        let start_progress =
            ((start_width - COLLAPSED_SIDEBAR_WIDTH).0 / width_range.0).clamp(0., 1.);
        let row_width = expanded_width - SIDEBAR_CONTENT_PAD * 2.;
        let target_progress = if collapsed { 0. } else { 1. };
        let row_animation = Animation::new(animation_duration).with_easing(ease_out_quint());
        let transition = RowTransition {
            generation: animation_id,
            animation: row_animation.clone(),
            start_progress,
            target_progress,
            row_width,
        };
        let nav_rows: Vec<_> = NAV_ITEMS
            .iter()
            .zip(&self.nav_focus)
            .map(|(item, focus_handle)| {
                self.nav_item(item, focus_handle, &transition, palette, window, cx)
            })
            .collect();
        let library = self.library.read(cx);
        let playlists = if library.playlists().is_empty() {
            library.pinned_playlists().clone()
        } else {
            library.playlists().clone()
        };
        let pins = library.pinned_playlists().clone();
        let catalog_changed = !Arc::ptr_eq(&self.playlist_source, &playlists);
        let pins_changed = !Arc::ptr_eq(&self.pinned_source, &pins)
            && self.pinned_source.as_ref() != pins.as_ref();
        if catalog_changed || pins_changed {
            // Rebuild only when either Arc changes, never per hover/frame.
            let pinned_keys: HashSet<_> = pins
                .iter()
                .map(|p| (p.provider, p.source_id.as_str()))
                .collect();
            let index_at = |position| {
                if catalog_changed {
                    position
                } else {
                    self.playlist_order[position]
                }
            };
            let is_pinned = |index: &usize| {
                pinned_keys.contains(&(
                    playlists[*index].provider,
                    playlists[*index].source_id.as_str(),
                ))
            };
            let mut order = Vec::with_capacity(playlists.len());
            order.extend((0..playlists.len()).map(index_at).filter(is_pinned));
            self.pinned_count = order.len();
            order.extend(
                (0..playlists.len())
                    .map(index_at)
                    .filter(|index| !is_pinned(index)),
            );
            self.playlist_source = playlists.clone();
            self.pinned_source = pins;
            self.playlist_order = order.into();
            self.playlist_hover = components::HoverHighlight::default();
            self.reorder_started = None;
            self.reorder_generation = 0;
        } else {
            // Local favorite/history writes can return an equal new pin snapshot.
            // Adopt its Arc without rebuilding the playlist order.
            self.pinned_source = pins;
        }
        let pinned_count = self.pinned_count;
        let order = self.playlist_order.clone();
        let active_playlist = if self.route == Route::Playlist {
            self.active_playlist.clone()
        } else {
            None
        };
        let reorder_generation = self.reorder_generation;
        let reorder_position = self.reorder_position;
        let reorder_destination = self.reorder_destination;
        let show_pinned = !playlists.is_empty();
        let reduce_motion = cx.reduce_motion();
        let reorder_progress = if reduce_motion {
            1.
        } else {
            self.reorder_started.map_or(1., |start| {
                (cx.background_executor()
                    .now()
                    .saturating_duration_since(start)
                    .as_secs_f32()
                    / 0.14)
                    .clamp(0., 1.)
            })
        };
        if reorder_progress < 1. {
            window.request_animation_frame();
        }
        let reorder_progress = ease_out_quint()(reorder_progress);
        let pinned_section = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .when(!collapsed, |section| {
                section.child(components::section_label(palette, "Playlists"))
            })
            .child(
                uniform_list(
                    "sidebar-playlists",
                    playlists.len(),
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        range
                            .filter_map(|position| {
                                let index = order[position];
                                playlists.get(index).cloned().map(|playlist| {
                                    let selected_playlist = playlist.clone();
                                    let label = playlist.name.clone();
                                    let selected = active_playlist.as_deref()
                                        == Some(playlist.source_id.as_str());
                                    let base = if selected {
                                        palette.selection
                                    } else {
                                        palette.canvas
                                    };
                                    let pinned = position < pinned_count;
                                    let row = components::row_button(
                                        palette,
                                        (
                                            if pinned {
                                                "pinned-playlist"
                                            } else {
                                                "sidebar-playlist"
                                            },
                                            index,
                                        ),
                                    )
                                    .test_support()
                                    .h_11()
                                    .w_full()
                                    .min_w_0()
                                    .when(!collapsed, |row| row.px_3())
                                    .bg(rgb(base))
                                    .aria_selected(selected)
                                    .rounded_lg()
                                    .justify_start()
                                    .when(collapsed, |row| row.justify_center())
                                    .gap_2()
                                    .text_sm()
                                    .text_color(rgb(palette.text))
                                    .child(components::artwork(
                                        palette,
                                        &this.image_cache,
                                        playlist.artwork_url.as_deref(),
                                        tokens::SIDEBAR_PLAYLIST_ARTWORK,
                                        CadenceIcon::Playlist,
                                    ))
                                    .when(pinned && !collapsed, |row| {
                                        row.child(components::icon(
                                            CadenceIcon::PinFilled,
                                            Rems(0.625),
                                            palette.text_muted,
                                        ))
                                    })
                                    .when(!collapsed, |row| {
                                        row.child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_ellipsis()
                                                .line_clamp(1)
                                                .child(playlist.name),
                                        )
                                    })
                                    .when(collapsed, |row| {
                                        rail_tooltip(
                                            row,
                                            label.clone(),
                                            this.tooltip_anchor.clone(),
                                        )
                                    })
                                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                        this.playlist_hover.set(
                                            index,
                                            *hovered,
                                            cx.background_executor().now(),
                                        );
                                        cx.notify();
                                    }))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(position) =
                                            this.playlist_order.iter().position(|id| *id == index)
                                        {
                                            this.promote_playlist(position, cx);
                                        }
                                        cx.emit(SidebarEvent::OpenPlaylist {
                                            playlist: selected_playlist.clone(),
                                            origin: pinned_origin,
                                        })
                                    }));
                                    let row = if let Some(hover) = this
                                        .playlist_hover
                                        .transition(index, cx.background_executor().now())
                                    {
                                        hover.apply(
                                            row,
                                            window,
                                            base,
                                            palette.control_hover,
                                            reduce_motion,
                                        )
                                    } else {
                                        row.into_any_element()
                                    };
                                    let offset = if reorder_generation == 0 {
                                        0.
                                    } else if position == reorder_destination {
                                        (reorder_position - reorder_destination) as f32
                                    } else if position > reorder_destination
                                        && position <= reorder_position
                                    {
                                        -1.
                                    } else {
                                        0.
                                    };
                                    div()
                                        .relative()
                                        .w_full()
                                        .top(Rems(offset * 2.75 * (1. - reorder_progress)))
                                        .child(row)
                                        .into_any_element()
                                })
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.playlist_scroll)
                .flex_1()
                .min_h_0(),
            );
        let toggle_focus_visible = components::is_focus_visible(&self.toggle_focus, window);
        let brand_fill = div()
            .h_12()
            .rounded_xl()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(tokens::BRAND_LABEL_GAP)
            .pr(BRAND_ROW_PAD)
            .when(toggle_focus_visible, |fill| {
                fill.shadow(components::inset_focus_ring(palette))
            })
            .hover(|style| style.bg(rgb(palette.control)))
            .child(
                img(self.brand_mark.clone())
                    .size(BRAND_LOGO_SIZE)
                    .flex_none(),
            )
            .child(div().whitespace_nowrap().child("Sonder").with_animation(
                ("sidebar-brand-label", animation_id),
                row_animation.clone(),
                move |label, delta| {
                    label.opacity(start_progress + (target_progress - start_progress) * delta)
                },
            ))
            .child(div().flex_1())
            .child(
                div()
                    .w(tokens::CONTROL_ICON)
                    .h_12()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(components::icon(
                        CadenceIcon::ChevronLeft,
                        tokens::CONTROL_ICON,
                        palette.text_primary,
                    ))
                    .with_animation(
                        ("sidebar-chevron", animation_id),
                        row_animation.clone(),
                        move |button, delta| {
                            button.opacity(
                                start_progress + (target_progress - start_progress) * delta,
                            )
                        },
                    ),
            )
            .with_animation(
                ("sidebar-brand-fill", animation_id),
                row_animation.clone(),
                move |fill, delta| {
                    let progress = start_progress + (target_progress - start_progress) * delta;
                    let (width, left, pad) =
                        sidebar_fill_geometry(BRAND_ROW_PAD, BRAND_LOGO_SIZE, row_width, progress);
                    fill.w(width).ml(left).pl(pad)
                },
            );
        let brand = components::bare_button("sidebar-toggle")
            .test_support()
            .track_focus(&self.toggle_focus)
            .h_12()
            .w_full()
            .flex_none()
            .justify_start()
            .items_center()
            .text_color(rgb(palette.text_primary))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .child(brand_fill)
            .tooltip_show_delay(Duration::ZERO)
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(if collapsed {
                    "Expand sidebar"
                } else {
                    "Collapse sidebar"
                })
                .build(window, cx)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                let collapsed = !this.collapsed;
                this.set_collapsed(collapsed, cx);
            }));

        div()
            .w(target_width)
            .h_full()
            .flex_none()
            .overflow_hidden()
            .bg(rgb(palette.canvas))
            .border_r_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .w(target_width)
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p(SIDEBAR_CONTENT_PAD)
                    .pt(tokens::SIDEBAR_TOP_INSET)
                    .child(brand)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(!collapsed, |nav| {
                                nav.child(
                                    div()
                                        .px_3()
                                        .pb_1()
                                        .child(components::section_label(palette, "Library"))
                                        .with_animation(
                                            ("sidebar-library-label", animation_id),
                                            row_animation.clone(),
                                            move |label, delta| {
                                                label.opacity(
                                                    start_progress
                                                        + (target_progress - start_progress)
                                                            * delta,
                                                )
                                            },
                                        ),
                                )
                            })
                            .children(nav_rows),
                    )
                    .when(show_pinned, |sidebar| {
                        sidebar.child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .child(pinned_section)
                                .with_animation(
                                    ("sidebar-pinned", animation_id),
                                    row_animation.clone(),
                                    move |pinned, delta| {
                                        pinned.opacity(if collapsed {
                                            1.
                                        } else {
                                            start_progress + (1. - start_progress) * delta
                                        })
                                    },
                                ),
                        )
                    })
                    .when(!show_pinned, |panel| panel.child(div().flex_1())),
            )
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.panel(window, cx)
    }
}

/// Native pointer tooltips use the pointer's bottom-right corner. Rail labels
/// instead follow the trigger's current layout, including scrolling/reordering.
/// One view exists only while hovered; no animation or timer is added.
fn rail_tooltip<E: StatefulInteractiveElement + gpui_kit::component::ElementExt>(
    row: E,
    label: SharedString,
    bounds: Rc<Cell<Bounds<Pixels>>>,
) -> E {
    let writer = bounds.clone();
    row.on_prepaint(move |value, window, _| {
        if value.contains(&window.mouse_position()) {
            writer.set(value);
        }
    })
    .tooltip_show_delay(Duration::ZERO)
    .tooltip(move |_, cx| {
        cx.new(|_| RailTooltip {
            bounds: bounds.clone(),
            label: label.clone(),
        })
        .into()
    })
}

struct RailTooltip {
    bounds: Rc<Cell<Bounds<Pixels>>>,
    label: SharedString,
}

impl Render for RailTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let bounds = self.bounds.get();
        gpui_kit::base::Positioner::side(bounds)
            .placement(gpui_kit::base::Placement::Right)
            .offset(px(8.))
            .child(
                div()
                    .id("sidebar-tooltip")
                    .test_support()
                    .max_w(Rems(22.))
                    .py(Rems(0.375))
                    .px_3()
                    .line_height(Rems(1.25))
                    .rounded_md()
                    .bg(rgb(palette.surface_raised))
                    .text_color(rgb(palette.text_primary))
                    .text_size(Rems(0.875))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(div().max_w(Rems(20.5)).child(self.label.clone())),
            )
    }
}
