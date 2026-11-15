//! Small reusable building blocks for the island UI.

use crate::assets::Icon;
use crate::theme::{self, c, color};
use gpui::prelude::*;
use gpui::{
    AnyView, App, Div, ElementId, FontWeight, Hsla, SharedString, Stateful, Svg, Window, div, px, svg,
};

pub fn icon(icon: Icon, size: f32, tint: Hsla) -> Svg {
    svg().path(icon.path()).size(px(size)).flex_none().text_color(tint)
}

/// A rounded island container (Fleet card).
pub fn island() -> Div {
    div()
        .flex()
        .flex_col()
        .bg(color(c::ISLAND))
        .border_1()
        .border_color(color(c::BORDER))
        .rounded(px(theme::ISLAND_RADIUS))
        .overflow_hidden()
}

/// Square icon button used in headers and the title bar.
pub fn icon_button(id: impl Into<ElementId>, ic: Icon, tint: Hsla) -> Stateful<Div> {
    div()
        .id(id.into())
        // Title-bar buttons sit inside the window Drag area: occlude so hit-testing
        // (and the OS caption logic on Windows) targets the button, not the drag region.
        .occlude()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(color(c::HOVER)))
        .active(|s| s.bg(color(c::PRESSED)))
        .child(icon(ic, 16., tint))
}

pub fn small_icon_button(id: impl Into<ElementId>, ic: Icon, tint: Hsla) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|s| s.bg(color(c::HOVER)))
        .active(|s| s.bg(color(c::PRESSED)))
        .child(icon(ic, 14., tint))
}

/// Horizontal text tab used in every island header.
pub fn header_tab(id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool, island_focused: bool) -> Stateful<Div> {
    let base = div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .px(px(9.))
        .rounded(px(6.))
        .border_1()
        .border_color(gpui::transparent_black())
        .text_size(px(theme::UI_TEXT))
        .cursor_pointer()
        .child(label.into());
    if active && island_focused {
        base.bg(color(c::TAB_FOCUSED)).border_color(color(c::TAB_FOCUSED_BORDER)).text_color(color(c::TEXT))
    } else if active {
        base.bg(color(c::TAB_ACTIVE)).text_color(color(c::TEXT))
    } else {
        base.text_color(color(c::TEXT_MUTED)).hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT)))
    }
}

/// Island header strip (tabs on the left, tools on the right).
pub fn island_header() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .h(px(theme::ISLAND_HEADER_HEIGHT))
        .px(px(6.))
        .border_b_1()
        .border_color(color(c::BORDER))
}

/// Keyboard shortcut pill.
pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(22.))
        .h(px(22.))
        .px(px(6.))
        .rounded(px(5.))
        .bg(color(c::PRESSED))
        .text_size(px(theme::UI_SMALL))
        .text_color(color(c::TEXT_MUTED))
        .child(text.into())
}

/// Thin vertical separator.
pub fn vsep() -> Div {
    div().flex_none().w(px(1.)).h(px(16.)).mx(px(6.)).bg(color(c::BORDER))
}

pub struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(color(c::POPUP))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .shadow_md()
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_SMALL))
            .text_color(color(c::TEXT))
            .child(self.text.clone())
            .children(self.shortcut.clone().map(|s| div().text_color(color(c::TEXT_DIM)).child(s)))
    }
}

pub fn tooltip(text: impl Into<SharedString>, shortcut: Option<&'static str>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_window, cx| cx.new(|_| Tooltip { text: text.clone(), shortcut: shortcut.map(SharedString::from) }).into()
}

/// Matched-character emphasis for fuzzy results: returns (text, bold ranges as char idx).
pub fn highlighted_label(text: &str, indices: &[u32], base: Hsla, emphasis: Hsla) -> Div {
    let mut row = div().flex().flex_none().overflow_hidden().whitespace_nowrap();
    let mut current = String::new();
    let mut current_hit = false;
    let flush = |row: Div, s: &mut String, hit: bool| -> Div {
        if s.is_empty() {
            return row;
        }
        let piece = std::mem::take(s);
        if hit {
            row.child(div().text_color(emphasis).font_weight(FontWeight::BOLD).child(piece))
        } else {
            row.child(div().text_color(base).child(piece))
        }
    };
    for (i, ch) in text.chars().enumerate() {
        let hit = indices.binary_search(&(i as u32)).is_ok();
        if hit != current_hit {
            row = flush(row, &mut current, current_hit);
            current_hit = hit;
        }
        current.push(ch);
    }
    flush(row, &mut current, current_hit)
}
