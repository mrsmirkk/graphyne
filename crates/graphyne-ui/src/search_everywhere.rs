//! Search Everywhere (Double Shift / Ctrl+K): floating card over the workspace with
//! CamelHump fuzzy ranking over files, symbols of the active file and IDE actions.

use crate::assets::Icon;
use crate::components::{highlighted_label, icon, kbd};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{self, c, color};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, Window, div, px, uniform_list};
use graphyne_core::fuzzy::fuzzy_filter;
use graphyne_syntax::{OutlineItem, SymbolKind};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdeAction {
    ToggleLeft,
    ToggleBottom,
    ToggleRight,
    Run,
    NewTerminal,
    DetachTerminal,
    FindInFiles,
    SaveAll,
    GitPanel,
    History,
    OpenFolder,
    CloseProject,
    Plugins,
    Quit,
}

const ACTIONS: &[(&str, &str, IdeAction)] = &[
    ("Run Current File", "Shift+F10", IdeAction::Run),
    ("Toggle Left Panel", "Ctrl+1", IdeAction::ToggleLeft),
    ("Toggle Bottom Panel", "Ctrl+J", IdeAction::ToggleBottom),
    ("Toggle Right Panel", "Ctrl+Alt+B", IdeAction::ToggleRight),
    ("New Terminal", "Ctrl+Shift+T", IdeAction::NewTerminal),
    ("Detach / Attach Terminal", "Alt+Shift+F12", IdeAction::DetachTerminal),
    ("Find in Files", "Ctrl+Shift+F", IdeAction::FindInFiles),
    ("Save All", "Ctrl+Alt+S", IdeAction::SaveAll),
    ("Git: Commit Changes", "", IdeAction::GitPanel),
    ("Git: Show History", "", IdeAction::History),
    ("Open Folder…", "", IdeAction::OpenFolder),
    ("Close Project", "", IdeAction::CloseProject),
    ("Plugins", "", IdeAction::Plugins),
    ("Exit", "Ctrl+Q", IdeAction::Quit),
];

#[derive(Clone, Debug)]
pub enum SearchItem {
    File(String),
    Symbol { row: usize },
    Action(IdeAction),
}

pub enum SearchEvent {
    Confirm(SearchItem),
    Dismiss,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Files,
    Symbols,
    Actions,
}

#[derive(Clone)]
struct Entry {
    item: SearchItem,
    label: String,
    detail: String,
    indices: Vec<u32>,
    shortcut: Option<&'static str>,
    section: &'static str,
    symbol_kind: Option<SymbolKind>,
}

pub struct SearchEverywhere {
    input: Entity<TextInput>,
    files: Arc<Vec<String>>,
    symbols: Arc<Vec<OutlineItem>>,
    entries: Vec<Entry>,
    selected: usize,
    tab: Tab,
    scroll: UniformListScrollHandle,
    _sub: Subscription,
}

impl EventEmitter<SearchEvent> for SearchEverywhere {}

impl Focusable for SearchEverywhere {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl SearchEverywhere {
    pub fn new(files: Arc<Vec<String>>, symbols: Arc<Vec<OutlineItem>>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Search files, symbols and actions", cx));
        let sub = cx.subscribe(&input, |this, _, ev, cx| match ev {
            TextInputEvent::Changed(_) => this.update_results(cx),
            TextInputEvent::Submit => this.confirm(cx),
            TextInputEvent::Cancel => cx.emit(SearchEvent::Dismiss),
            TextInputEvent::Up => this.select_delta(-1, cx),
            TextInputEvent::Down => this.select_delta(1, cx),
        });
        let mut this = Self {
            input,
            files,
            symbols,
            entries: Vec::new(),
            selected: 0,
            tab: Tab::All,
            scroll: UniformListScrollHandle::new(),
            _sub: sub,
        };
        this.update_results(cx);
        this
    }

    pub fn set_files(&mut self, files: Arc<Vec<String>>, cx: &mut Context<Self>) {
        self.files = files;
        self.update_results(cx);
    }

    fn select_delta(&mut self, d: isize, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as isize;
        self.selected = ((self.selected as isize + d).rem_euclid(n)) as usize;
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.entries.get(self.selected) {
            cx.emit(SearchEvent::Confirm(e.item.clone()));
        }
    }

    fn update_results(&mut self, cx: &mut Context<Self>) {
        let q = self.input.read(cx).text().trim().to_string();
        let mut entries = Vec::new();
        let want = |t: Tab| self.tab == Tab::All || self.tab == t;
        if want(Tab::Files) {
            let limit = if self.tab == Tab::Files { 200 } else { 40 };
            for m in fuzzy_filter(&self.files, &q, limit, true) {
                let rel = &self.files[m.index];
                let (dir, name) = match rel.rfind('/') {
                    Some(i) => (&rel[..i], &rel[i + 1..]),
                    None => ("", rel.as_str()),
                };
                let name_start = rel.chars().count() - name.chars().count();
                let name_indices: Vec<u32> = m.indices.iter().filter(|i| **i as usize >= name_start).map(|i| i - name_start as u32).collect();
                entries.push(Entry {
                    item: SearchItem::File(rel.clone()),
                    label: name.to_string(),
                    detail: dir.to_string(),
                    indices: name_indices,
                    shortcut: None,
                    section: "Files",
                    symbol_kind: None,
                });
            }
        }
        if want(Tab::Symbols) && !q.is_empty() || self.tab == Tab::Symbols {
            let names: Vec<String> = self.symbols.iter().map(|s| s.name.clone()).collect();
            for m in fuzzy_filter(&names, &q, 30, false) {
                let s = &self.symbols[m.index];
                entries.push(Entry {
                    item: SearchItem::Symbol { row: s.row },
                    label: s.name.clone(),
                    detail: format!("line {}", s.row + 1),
                    indices: m.indices,
                    shortcut: None,
                    section: "Symbols",
                    symbol_kind: Some(s.kind),
                });
            }
        }
        if want(Tab::Actions) {
            let labels: Vec<&str> = ACTIONS.iter().map(|a| a.0).collect();
            for m in fuzzy_filter(&labels, &q, 20, false) {
                let (label, shortcut, action) = ACTIONS[m.index];
                entries.push(Entry {
                    item: SearchItem::Action(action),
                    label: label.to_string(),
                    detail: String::new(),
                    indices: m.indices,
                    shortcut: (!shortcut.is_empty()).then_some(shortcut),
                    section: "Actions",
                    symbol_kind: None,
                });
            }
        }
        self.entries = entries;
        self.selected = 0;
        cx.notify();
    }
}

impl Render for SearchEverywhere {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.entries.clone();
        let selected = self.selected;
        let tab = self.tab;
        let tabs = [(Tab::All, "All"), (Tab::Files, "Files"), (Tab::Symbols, "Symbols"), (Tab::Actions, "Actions")];
        div()
            .id("search-everywhere")
            .occlude()
            .w(px(700.))
            .max_h(px(520.))
            .flex()
            .flex_col()
            .rounded(px(12.))
            .bg(color(c::POPUP))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0., 0., 0., 0.55),
                offset: gpui::point(px(0.), px(18.)),
                blur_radius: px(48.),
                spread_radius: px(0.),
            }])
            .overflow_hidden()
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_TEXT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .px(px(8.))
                    .pt(px(8.))
                    .children(tabs.into_iter().map(|(t, label)| {
                        crate::components::header_tab(SharedString::from(format!("se-tab-{label}")), label, t == tab, true).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.tab = t;
                                this.update_results(cx);
                            },
                        ))
                    }))
                    .child(div().flex_1())
                    .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child("Double Shift")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(44.))
                    .mx(px(8.))
                    .my(px(8.))
                    .px(px(12.))
                    .rounded(px(8.))
                    .bg(color(c::CANVAS))
                    .border_1()
                    .border_color(color(c::TAB_FOCUSED_BORDER))
                    .child(icon(Icon::Search, 16., color(c::TEXT_MUTED)))
                    .child(self.input.clone()),
            )
            .child(
                uniform_list("se-results", entries.len(), cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| {
                            let e = &entries[ix];
                            let is_sel = ix == selected;
                            let first_in_section = ix == 0 || entries[ix - 1].section != e.section;
                            let leading = match &e.item {
                                SearchItem::File(rel) => crate::file_icons::file_icon(std::path::Path::new(rel)).into_any_element(),
                                SearchItem::Symbol { .. } => div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(16.))
                                    .rounded(px(4.))
                                    .bg(color(0x3A2F52))
                                    .text_color(color(0xC5A3F0))
                                    .text_size(px(10.))
                                    .child(e.symbol_kind.map(|k| k.badge()).unwrap_or("ƒ"))
                                    .into_any_element(),
                                SearchItem::Action(_) => icon(Icon::Bolt, 14., color(c::ACCENT)).into_any_element(),
                            };
                            div()
                                .id(ix)
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .h(px(30.))
                                .w_full()
                                .px(px(10.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .when(is_sel, |d| d.bg(color(c::ACCENT_SOFT)))
                                .when(!is_sel, |d| d.hover(|s| s.bg(color(c::HOVER))))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected = ix;
                                    this.confirm(cx);
                                }))
                                .child(leading)
                                .child(highlighted_label(&e.label, &e.indices, color(c::TEXT), color(c::MATCH)))
                                .child(div().flex_1().min_w(px(0.)).whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(12.)).text_color(color(c::TEXT_DIM)).child(e.detail.clone()))
                                .children(e.shortcut.map(|s| div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child(s)))
                                .when(first_in_section, |d| d.child(div().text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(e.section)))
                        })
                        .collect()
                }))
                .track_scroll(self.scroll.clone())
                .px(px(6.))
                .h(px(380.)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .h(px(30.))
                    .px(px(14.))
                    .border_t_1()
                    .border_color(color(c::BORDER))
                    .text_size(px(11.5))
                    .text_color(color(c::TEXT_DIM))
                    .child(div().flex().items_center().gap(px(6.)).child(kbd("↑↓")).child("navigate"))
                    .child(div().flex().items_center().gap(px(6.)).child(kbd("Enter")).child("open"))
                    .child(div().flex().items_center().gap(px(6.)).child(kbd("Esc")).child("close"))
                    .child(div().flex_1())
                    .child(format!("{} files indexed", self.files.len())),
            )
    }
}
