//! Welcome window: shown when Graphyne starts with nothing to reopen, and after
//! Close Project. Open a folder or one of the recent projects.

use crate::assets::Icon;
use crate::components::{icon, kbd, small_icon_button, tooltip};
use crate::theme::{self, c, color, color_a};
use crate::workspace::caption_buttons;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, KeyDownEvent, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowControlArea, WindowDecorations, WindowOptions, div, point, px, size,
};
use std::path::PathBuf;

pub struct Welcome {
    focus_handle: FocusHandle,
    recent: Vec<PathBuf>,
    /// Keyboard selection in the recent list.
    selected: usize,
    hovered: Option<usize>,
}

impl Focusable for Welcome {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Show the Welcome window (or bring the existing one forward).
pub fn open_welcome(cx: &mut App) {
    if let Some(existing) = cx.windows().into_iter().find_map(|w| w.downcast::<Welcome>()) {
        existing.update(cx, |_, window, _| window.activate_window()).ok();
        return;
    }
    let bounds = Bounds::centered(None, size(px(880.), px(580.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Welcome to Graphyne".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(13.))),
        }),
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(size(px(640.), px(440.))),
        app_id: Some("graphyne".into()),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        crate::window_opened();
        window.on_window_should_close(cx, |_, cx| {
            crate::window_closed(cx);
            true
        });
        cx.new(|cx| Welcome::new(window, cx))
    });
    match opened {
        Ok(_) => cx.activate(true),
        Err(err) => {
            log::error!("failed to open the Welcome window: {err:#}");
            if cx.windows().is_empty() {
                cx.quit();
            }
        }
    }
}

/// Close the Welcome window once a project window is open.
pub fn close_welcome(cx: &mut App) {
    // Deferred: the Welcome window may be the one handling this click.
    cx.defer(|cx| {
        if let Some(welcome) = cx.windows().into_iter().find_map(|w| w.downcast::<Welcome>()) {
            if welcome.update(cx, |_, window, _| window.remove_window()).is_ok() {
                crate::window_closed(cx);
            }
        }
    });
}

impl Welcome {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        Self { focus_handle, recent: graphyne_core::recent::load(), selected: 0, hovered: None }
    }

    fn open_recent(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self.recent.get(ix).cloned() else { return };
        if !path.is_dir() {
            // Moved or deleted since the list was loaded.
            self.forget(ix, cx);
            return;
        }
        crate::open_workspace(path, |_| {}, cx);
    }

    fn forget(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.recent.len() {
            return;
        }
        let path = self.recent.remove(ix);
        self.selected = self.selected.min(self.recent.len().saturating_sub(1));
        self.hovered = None;
        cx.background_spawn(async move {
            if let Err(e) = graphyne_core::recent::remove(&path) {
                log::warn!("recent projects: {e:#}");
            }
        })
        .detach();
        cx.notify();
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let n = self.recent.len();
        match k.key.as_str() {
            "o" if k.modifiers.control || k.modifiers.platform => crate::prompt_open_folder(cx),
            "down" if n > 0 => self.selected = (self.selected + 1) % n,
            "up" if n > 0 => self.selected = (self.selected + n - 1) % n,
            "home" => self.selected = 0,
            "end" => self.selected = n.saturating_sub(1),
            "enter" if n > 0 => self.open_recent(self.selected, cx),
            "delete" if n > 0 => self.forget(self.selected, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn recent_row(&self, ix: usize, path: &PathBuf, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let name = graphyne_core::project::project_name(path);
        let letter: SharedString = name.chars().next().map(|ch| ch.to_uppercase().to_string()).unwrap_or_default().into();
        let tint = avatar_color(&name);
        let selected = ix == self.selected;
        let show_remove = self.hovered == Some(ix) || selected;
        div()
            .id(("recent", ix))
            .flex()
            .items_center()
            .gap(px(12.))
            .h(px(54.))
            .px(px(12.))
            .rounded(px(8.))
            .cursor_pointer()
            .when(selected, |d| d.bg(color(c::HOVER)))
            .when(!selected, |d| d.hover(|s| s.bg(color_a(c::HOVER, 0.6))))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered = Some(ix);
                } else if this.hovered == Some(ix) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = ix;
                this.open_recent(ix, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(34.))
                    .rounded(px(8.))
                    .bg(color_a(tint, 0.22))
                    .border_1()
                    .border_color(color_a(tint, 0.45))
                    .text_size(px(15.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(color(tint))
                    .child(letter),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(2.))
                    .child(div().text_color(color(c::TEXT)).font_weight(gpui::FontWeight::MEDIUM).whitespace_nowrap().overflow_hidden().text_ellipsis().child(name))
                    .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(path.display().to_string())),
            )
            .when(show_remove, |d| {
                d.child(
                    small_icon_button(("recent-remove", ix), Icon::Close, color(c::TEXT_DIM))
                        .tooltip(tooltip("Remove from the list", Some("Delete")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.forget(ix, cx);
                        })),
                )
            })
    }
}

/// A steady colour per project name for its letter badge.
fn avatar_color(name: &str) -> u32 {
    const PALETTE: [u32; 6] = [c::ACCENT, c::GREEN, c::ORANGE, 0xB07CE8, 0x4FB6C4, c::YELLOW];
    let h = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[h as usize % PALETTE.len()]
}

impl Render for Welcome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title_bar = div()
            .id("welcome-title")
            .flex()
            .flex_none()
            .items_center()
            .h(px(theme::TITLE_BAR_HEIGHT))
            .pl(px(if cfg!(target_os = "macos") { 80. } else { 16. }))
            .window_control_area(WindowControlArea::Drag)
            .child(div().text_size(px(theme::UI_SMALL)).text_color(color(c::TEXT_DIM)).child("Welcome to Graphyne"))
            .child(div().flex_1())
            .when(!cfg!(target_os = "macos"), |d| d.child(caption_buttons(window.is_maximized(), cx)));

        let open_button = div()
            .id("welcome-open")
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(36.))
            .px(px(14.))
            .rounded(px(8.))
            .cursor_pointer()
            .bg(color(c::ACCENT))
            .hover(|s| s.bg(color_a(c::ACCENT, 0.85)))
            .text_color(color(0xFFFFFF))
            .child(icon(Icon::FolderFill, 16., color(0xFFFFFF)))
            .child(div().flex_1().child("Open Folder…"))
            .child(div().text_size(px(11.5)).text_color(color_a(0xFFFFFF, 0.75)).child("Ctrl+O"))
            .on_click(cx.listener(|_, _, _, cx| crate::prompt_open_folder(cx)));
        let plugins_button = div()
            .id("welcome-plugins")
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(36.))
            .px(px(14.))
            .rounded(px(8.))
            .cursor_pointer()
            .text_color(color(c::TEXT_MUTED))
            .hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT)))
            .child(icon(Icon::Puzzle, 16., color(c::TEXT_MUTED)))
            .child("Plugins folder")
            .on_click(cx.listener(|_, _, _, cx| {
                let dir = crate::lua_bridge::plugins_dir(cx);
                std::fs::create_dir_all(&dir).ok();
                cx.reveal_path(&dir);
            }));

        let sidebar = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(280.))
            .h_full()
            .p(px(24.))
            .bg(color(c::ISLAND))
            .rounded(px(theme::ISLAND_RADIUS))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(44.))
                            .rounded(px(10.))
                            .bg(color(c::ACCENT_SOFT))
                            .child(icon(Icon::Sparkle, 26., color(c::ACCENT))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(18.)).text_color(color(c::TEXT)).font_weight(gpui::FontWeight::SEMIBOLD).child("Graphyne"))
                            .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(format!("version {}", env!("CARGO_PKG_VERSION")))),
                    ),
            )
            .child(div().h(px(28.)))
            .child(div().flex().flex_col().gap(px(4.)).child(open_button).child(plugins_button))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_size(px(11.5))
                    .text_color(color(c::TEXT_DIM))
                    .child(div().flex().items_center().gap(px(6.)).child(kbd("↑")).child(kbd("↓")).child(kbd("Enter")).child("open a recent project"))
                    .child(div().flex().items_center().gap(px(6.)).child(kbd("Delete")).child("remove it from the list")),
            );

        let count = self.recent.len();
        let rows: Vec<_> = self.recent.clone().iter().enumerate().map(|(ix, p)| self.recent_row(ix, p, cx).into_any_element()).collect();
        let list = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .pl(px(28.))
            .pr(px(20.))
            .pt(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pb(px(10.))
                    .px(px(12.))
                    .child(div().text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child("RECENT PROJECTS"))
                    .when(count > 0, |d| d.child(div().text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(count.to_string()))),
            )
            .when(count == 0, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap(px(10.))
                        .pb(px(60.))
                        .child(icon(Icon::Folder, 34., color(c::TEXT_FAINT)))
                        .child(div().text_color(color(c::TEXT_MUTED)).child("No recent projects"))
                        .child(div().text_size(px(12.)).text_color(color(c::TEXT_DIM)).child("Open a folder to get started — it will show up here.")),
                )
            })
            .when(count > 0, |d| d.child(div().id("recent-list").flex().flex_col().gap(px(2.)).flex_1().overflow_y_scroll().children(rows)));

        div()
            .id("welcome")
            .key_context("Welcome")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(color(c::CANVAS))
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_TEXT))
            .text_color(color(c::TEXT))
            .child(title_bar)
            .child(div().flex().flex_1().min_h(px(0.)).px(px(theme::GAP + 3.)).pb(px(theme::GAP + 3.)).child(sidebar).child(list))
    }
}
