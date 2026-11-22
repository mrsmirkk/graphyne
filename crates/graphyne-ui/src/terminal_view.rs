//! Terminal island: GPUI views over `graphyne_terminal::TerminalState`.
//!
//! * [`TerminalView`] — one PTY session (renders the alacritty grid on the GPU).
//! * [`TerminalPanel`] — horizontal tabs + `+` + Detach, used both embedded in the bottom
//!   island and inside the detached OS window ([`DetachedTerminal`]).

use crate::actions::terminal as term_actions;
use crate::assets::Icon;
use crate::components::{icon, island, island_header, small_icon_button, tooltip};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, FontStyle, FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point,
    ScrollWheelEvent, SharedString, Subscription, Task, TextRun, UTF16Selection, UnderlineStyle, WeakEntity, Window, canvas,
    div, fill, point, px, rgb, size,
};
use graphyne_terminal::{KeyInput, SelectionKind, SpawnOptions, TerminalSnapshot, TerminalState, key_to_bytes};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:[A-Za-z]:)?[\w.\-/\\]+\.[A-Za-z][A-Za-z0-9]{0,5}:\d+(?::\d+)?").expect("valid link regex"))
}

fn to_hsla(c: graphyne_terminal::Rgb) -> Hsla {
    rgb(c.to_u32()).into()
}

#[derive(Clone, Copy)]
struct TermLayout {
    origin: Point<Pixels>,
    cell_w: Pixels,
    cell_h: Pixels,
}

pub enum TerminalViewEvent {
    OpenFile(PathBuf, usize, usize),
    Exited,
}

pub struct TerminalView {
    focus_handle: FocusHandle,
    pub state: Option<Arc<TerminalState>>,
    pub error: Option<String>,
    cwd: PathBuf,
    pending_input: Vec<u8>,
    /// The user typed or pasted something here (the tab is in use: never restarted for them).
    pub typed: bool,
    layout: Option<TermLayout>,
    last_snapshot: Option<Arc<TerminalSnapshot>>,
    selecting: bool,
    _spawn: Option<Task<()>>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    pub fn new(cwd: PathBuf, command: Option<(String, Vec<String>)>, env: Vec<(String, String)>, path_prepend: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        let opts = SpawnOptions { cwd: cwd.clone(), command, env, path_prepend, cols: 120, rows: 24 };
        // Spawning a PTY can take tens of ms (ConPTY): never on the UI thread.
        let spawn = cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { TerminalState::spawn(opts) }).await;
            let state = match result {
                Ok(state) => Arc::new(state),
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(format!("{err:#}"));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let wakeups = state.wakeups();
            let ok = this
                .update(cx, |this, cx| {
                    if !this.pending_input.is_empty() {
                        state.input(&std::mem::take(&mut this.pending_input));
                    }
                    this.state = Some(state.clone());
                    cx.notify();
                })
                .is_ok();
            if !ok {
                return;
            }
            while wakeups.recv_async().await.is_ok() {
                let alive = this
                    .update(cx, |this, cx| {
                        if this.state.as_ref().is_some_and(|s| s.has_exited()) {
                            cx.emit(TerminalViewEvent::Exited);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            state: None,
            error: None,
            cwd,
            pending_input: Vec::new(),
            typed: false,
            layout: None,
            last_snapshot: None,
            selecting: false,
            _spawn: Some(spawn),
        }
    }

    pub fn send_text(&mut self, text: &str) {
        match &self.state {
            Some(state) => state.input(text.as_bytes()),
            None => self.pending_input.extend_from_slice(text.as_bytes()),
        }
    }

    pub fn has_exited(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.has_exited())
    }

    pub fn title(&self) -> Option<String> {
        self.state.as_ref().and_then(|s| s.title())
    }

    fn cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let l = self.layout?;
        let x = (position.x - l.origin.x).max(px(0.));
        let y = (position.y - l.origin.y).max(px(0.));
        Some(((y / l.cell_h).floor() as usize, (x / l.cell_w).floor() as usize))
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.clone() else { return };
        let k = &event.keystroke;
        if k.modifiers.platform {
            return;
        }
        // Windows-terminal conventions: Ctrl+C copies when there is a selection, Ctrl+V pastes.
        if k.modifiers.control && !k.modifiers.shift && !k.modifiers.alt {
            if k.key == "c" {
                if let Some(text) = state.selection_text().filter(|t| !t.is_empty()) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    state.clear_selection();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
            }
            if k.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    self.typed = true;
                    state.paste(&text);
                }
                cx.stop_propagation();
                return;
            }
        }
        // Plain printable text (and AltGr = Ctrl+Alt characters) is left to the platform
        // text-input path (WM_CHAR / IME) and arrives via `EntityInputHandler` below.
        let printable = k.key_char.as_deref().is_some_and(|c| !c.chars().any(char::is_control));
        let altgr = k.modifiers.control && k.modifiers.alt;
        if printable && ((!k.modifiers.control && !k.modifiers.alt) || altgr) && k.key != "space" {
            return;
        }
        let input = KeyInput {
            key: &k.key,
            key_char: k.key_char.as_deref(),
            ctrl: k.modifiers.control,
            alt: k.modifiers.alt,
            shift: k.modifiers.shift,
        };
        if let Some(bytes) = key_to_bytes(input, state.app_cursor()) {
            self.typed = true;
            state.input(&bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        let Some(state) = self.state.clone() else { return };
        let Some((row, col)) = self.cell_at(event.position) else { return };
        if event.modifiers.control || event.modifiers.platform {
            if let Some(snap) = &self.last_snapshot {
                if let Some(text) = snap.row_text.get(row) {
                    if let Some((path, line, column)) = link_at(text, col, &self.cwd) {
                        cx.emit(TerminalViewEvent::OpenFile(path, line, column));
                        return;
                    }
                }
            }
        }
        let kind = match event.click_count {
            2 => SelectionKind::Word,
            n if n >= 3 => SelectionKind::Line,
            _ => SelectionKind::Simple,
        };
        state.start_selection(row, col, kind);
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        if let (Some(state), Some((row, col))) = (self.state.clone(), self.cell_at(event.position)) {
            state.update_selection(row, col);
            cx.notify();
        }
    }

    fn on_right_click(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.clone() else { return };
        match state.selection_text().filter(|t| !t.is_empty()) {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                state.clear_selection();
            }
            None => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    self.typed = true;
                    state.paste(&text);
                }
            }
        }
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.clone() else { return };
        let lh = px(theme::term_line_height());
        let dy = event.delta.pixel_delta(lh).y;
        let lines = (dy / lh).round() as i32;
        if lines != 0 {
            state.scroll(lines);
            cx.notify();
        }
    }

    fn copy(&mut self, _: &term_actions::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.state.as_ref().and_then(|s| s.selection_text()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
    fn paste(&mut self, _: &term_actions::Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(state), Some(text)) = (self.state.clone(), cx.read_from_clipboard().and_then(|i| i.text())) {
            self.typed = true;
            state.paste(&text);
        }
    }
    fn page_up(&mut self, _: &term_actions::ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = &self.state {
            s.scroll(20);
            cx.notify();
        }
    }
    fn page_down(&mut self, _: &term_actions::ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = &self.state {
            s.scroll(-20);
            cx.notify();
        }
    }
}

/// Every clickable `path:line[:col]` (compiler style) or `File "path", line N` (Python
/// tracebacks) in a row: (char span to underline, file, 1-based line, 1-based col).
fn find_links(text: &str) -> Vec<(std::ops::Range<usize>, String, usize, usize)> {
    static PY: OnceLock<Regex> = OnceLock::new();
    let py = PY.get_or_init(|| Regex::new(r#"File "([^"]+)", line (\d+)"#).expect("valid regex"));
    let chars = |b: usize| text[..b].chars().count();
    let mut out = Vec::new();
    for c in py.captures_iter(text) {
        let (Some(file), Some(line)) = (c.get(1), c.get(2)) else { continue };
        if let Ok(line) = line.as_str().parse() {
            out.push((chars(file.start())..chars(file.end()), file.as_str().to_string(), line, 1));
        }
    }
    for m in link_re().find_iter(text) {
        let mut parts = m.as_str().rsplitn(3, ':').collect::<Vec<_>>();
        parts.reverse();
        let parsed = match parts.as_slice() {
            [file, line, column] if line.parse::<usize>().is_ok() && column.parse::<usize>().is_ok() => {
                Some((file.to_string(), line.parse().unwrap_or(1), column.parse().unwrap_or(1)))
            }
            [file, line, column] if column.parse::<usize>().is_ok() => Some((format!("{file}:{line}"), column.parse().unwrap_or(1), 1)),
            [file, line] => line.parse().ok().map(|l| (file.to_string(), l, 1)),
            _ => None,
        };
        if let Some((file, line, col)) = parsed {
            out.push((chars(m.start())..chars(m.end()), file, line, col));
        }
    }
    out
}

fn link_at(text: &str, col: usize, cwd: &Path) -> Option<(PathBuf, usize, usize)> {
    let (_, file, line, column) = find_links(text).into_iter().find(|(span, ..)| span.contains(&col))?;
    let p = PathBuf::from(&file);
    let path = if p.is_absolute() { p } else { cwd.join(p) };
    Some((path, line.saturating_sub(1), column.saturating_sub(1)))
}

struct TermPaint {
    snapshot: Arc<TerminalSnapshot>,
    cell_w: Pixels,
    cell_h: Pixels,
    origin: Point<Pixels>,
    focused: bool,
    cursor_color: Hsla,
}

fn terminal_element(view: Entity<TerminalView>) -> impl IntoElement {
    let pre = view.clone();
    canvas(
        move |bounds, window, cx| {
            let font = gpui::font(theme::fonts().term);
            let font_size = px(theme::term_size());
            let text_system = window.text_system().clone();
            let font_id = text_system.resolve_font(&font);
            let cell_w = text_system.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(px(8.));
            let cell_h = px(theme::term_line_height());
            let origin = point(bounds.left() + px(10.), bounds.top() + px(6.));
            let cols = ((bounds.size.width - px(20.)) / cell_w).floor().max(10.) as u16;
            let rows = ((bounds.size.height - px(8.)) / cell_h).floor().max(2.) as u16;
            let v = pre.read(cx);
            let focused = v.focus_handle.is_focused(window);
            let snapshot = v.state.as_ref().map(|s| {
                s.resize(cols, rows, f32::from(cell_w) as u16, f32::from(cell_h) as u16);
                Arc::new(s.snapshot())
            });
            let cursor_color = v.state.as_ref().map(|s| to_hsla(s.palette().cursor)).unwrap_or(color(c::CURSOR));
            pre.update(cx, |v, _| {
                v.layout = Some(TermLayout { origin, cell_w, cell_h });
                v.last_snapshot = snapshot.clone();
            });
            snapshot.map(|snapshot| TermPaint { snapshot, cell_w, cell_h, origin, focused, cursor_color })
        },
        move |bounds, paint, window, cx| {
            let focus_handle = view.read(cx).focus_handle.clone();
            window.handle_input(&focus_handle, ElementInputHandler::new(bounds, view.clone()), cx);
            let Some(p) = paint else { return };
            let font = gpui::font(theme::fonts().term);
            let font_size = px(theme::term_size());
            let snap = &p.snapshot;
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                // Backgrounds.
                for (row, runs) in snap.rows.iter().enumerate() {
                    let y = p.origin.y + p.cell_h * row as f32;
                    for run in runs {
                        if let Some(bg) = run.bg {
                            let x = p.origin.x + p.cell_w * run.col as f32;
                            window.paint_quad(fill(Bounds::new(point(x, y), size(p.cell_w * run.cells as f32, p.cell_h)), to_hsla(bg)));
                        }
                    }
                }
                // Selection.
                if let Some(((sr, sc), (er, ec))) = snap.selection {
                    for row in sr..=er.min(snap.screen_lines.saturating_sub(1)) {
                        let c0 = if row == sr { sc } else { 0 };
                        let c1 = if row == er { ec + 1 } else { snap.cols };
                        let y = p.origin.y + p.cell_h * row as f32;
                        let x0 = p.origin.x + p.cell_w * c0 as f32;
                        let x1 = p.origin.x + p.cell_w * c1.max(c0) as f32;
                        window.paint_quad(fill(Bounds::from_corners(point(x0, y), point(x1, y + p.cell_h)), color(c::SELECTION)));
                    }
                }
                // Cursor (under the glyph).
                if let Some((row, col)) = snap.cursor {
                    let x = p.origin.x + p.cell_w * col as f32;
                    let y = p.origin.y + p.cell_h * row as f32;
                    let cell = Bounds::new(point(x, y), size(p.cell_w, p.cell_h));
                    if p.focused && snap.cursor_block {
                        window.paint_quad(fill(cell, p.cursor_color.opacity(0.85)));
                    } else if p.focused {
                        window.paint_quad(fill(Bounds::new(point(x, y + px(1.)), size(px(2.), p.cell_h - px(2.))), p.cursor_color));
                    } else {
                        window.paint_quad(gpui::outline(cell, p.cursor_color.opacity(0.6), gpui::BorderStyle::Solid));
                    }
                }
                // Text.
                for (row, runs) in snap.rows.iter().enumerate() {
                    let y = p.origin.y + p.cell_h * row as f32;
                    for run in runs {
                        if run.text.trim().is_empty() && !run.underline {
                            continue;
                        }
                        let mut f = font.clone();
                        if run.bold {
                            f.weight = FontWeight::BOLD;
                        }
                        if run.italic {
                            f.style = FontStyle::Italic;
                        }
                        let mut fg = to_hsla(run.fg);
                        if p.focused && snap.cursor_block && snap.cursor.is_some_and(|(cr, cc)| cr == row && cc >= run.col && cc < run.col + run.cells) && run.cells == 1 {
                            fg = color(c::ISLAND);
                        }
                        let tr = TextRun {
                            len: run.text.len(),
                            font: f,
                            color: fg,
                            background_color: None,
                            underline: run.underline.then(|| UnderlineStyle { color: Some(fg), thickness: px(1.), wavy: false }),
                            strikethrough: None,
                        };
                        let shaped = window.text_system().shape_line(run.text.clone().into(), font_size, &[tr], None);
                        let x = p.origin.x + p.cell_w * run.col as f32;
                        let _ = shaped.paint(point(x, y), p.cell_h, window, cx);
                    }
                    // Clickable file:line:col links (Ctrl+Click).
                    if let Some(text) = snap.row_text.get(row) {
                        for (span, ..) in find_links(text) {
                            let (s, e) = (span.start, span.end);
                            let x0 = p.origin.x + p.cell_w * s as f32;
                            window.paint_quad(fill(
                                Bounds::new(point(x0, y + p.cell_h - px(3.)), size(p.cell_w * (e - s) as f32, px(1.))),
                                color_a(c::MATCH, 0.55),
                            ));
                        }
                    }
                }
                // Scrollback indicator.
                if snap.display_offset > 0 && snap.history_size > 0 {
                    let h = bounds.size.height;
                    let ratio = snap.screen_lines as f32 / (snap.screen_lines + snap.history_size) as f32;
                    let thumb = (h * ratio).max(px(20.));
                    let pos = 1.0 - snap.display_offset as f32 / snap.history_size as f32;
                    let top = bounds.top() + (h - thumb) * pos;
                    window.paint_quad(fill(Bounds::new(point(bounds.right() - px(8.), top), size(px(5.), thumb)), color_a(c::TEXT, 0.18)).corner_radii(px(2.5)));
                }
            });
        },
    )
    .size_full()
}

/// Text input (typed characters, IME commits, AltGr symbols) goes straight to the PTY.
impl EntityInputHandler for TerminalView {
    fn text_for_range(&mut self, _: std::ops::Range<usize>, _: &mut Option<std::ops::Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(&mut self, _: Option<std::ops::Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        self.typed = true;
        self.send_text(text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        _: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        // IME composition preview is not rendered inside the grid; the committed text arrives
        // through `replace_text_in_range`.
    }

    fn bounds_for_range(&mut self, _: std::ops::Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let l = self.layout?;
        let (row, col) = self.last_snapshot.as_ref().and_then(|s| s.cursor).unwrap_or((0, 0));
        Some(Bounds::new(point(l.origin.x + l.cell_w * col as f32, l.origin.y + l.cell_h * row as f32), size(l.cell_w, l.cell_h)))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: gpui::AnyElement = if let Some(err) = &self.error {
            div()
                .p(px(16.))
                .text_color(color(c::RED))
                .font_family(theme::fonts().term)
                .text_size(px(12.5))
                .child(format!("Failed to start shell: {err}"))
                .into_any_element()
        } else if self.state.is_none() {
            div().p(px(12.)).text_color(color(c::TEXT_DIM)).child("Starting shell…").into_any_element()
        } else {
            terminal_element(cx.entity()).into_any_element()
        };
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .cursor_text()
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(body)
    }
}

// ---------------------------------------------------------------------------------------

pub enum TerminalPanelEvent {
    OpenFile(PathBuf, usize, usize),
    ToggleDetach,
    ShowDebug,
}

struct TermTab {
    title: SharedString,
    view: Entity<TerminalView>,
    is_run: bool,
    _sub: Subscription,
}

pub struct TerminalPanel {
    focus_handle: FocusHandle,
    tabs: Vec<TermTab>,
    active: usize,
    pub root: PathBuf,
    pub detached: bool,
    counter: usize,
    pub debugger: Option<Entity<crate::debugger::Debugger>>,
}

impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.tabs.get(self.active).map(|t| t.view.focus_handle(cx)).unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl TerminalPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle(), tabs: Vec::new(), active: 0, root, detached: false, counter: 0, debugger: None }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_view(&self) -> Option<Entity<TerminalView>> {
        self.tabs.get(self.active).map(|t| t.view.clone())
    }

    fn add_tab(&mut self, title: SharedString, is_run: bool, cwd: PathBuf, cx: &mut Context<Self>) -> Entity<TerminalView> {
        // Run tabs get commands in PowerShell syntax: they use that shell, not the user's.
        let command = is_run.then(graphyne_terminal::run_shell);
        self.add_tab_with(title, is_run, cwd, command, Vec::new(), cx)
    }

    fn add_tab_with(
        &mut self,
        title: SharedString,
        is_run: bool,
        cwd: PathBuf,
        command: Option<(String, Vec<String>)>,
        env: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let (view, sub) = self.make_view(cwd, command, env, cx);
        self.tabs.push(TermTab { title, view: view.clone(), is_run, _sub: sub });
        self.active = self.tabs.len() - 1;
        cx.notify();
        view
    }

    /// A terminal with the project's environment (`python` / `pip` are the project's
    /// interpreter), and the subscription that follows it.
    fn make_view(&mut self, cwd: PathBuf, command: Option<(String, Vec<String>)>, env: Vec<(String, String)>, cx: &mut Context<Self>) -> (Entity<TerminalView>, Subscription) {
        let (mut vars, path_prepend) = graphyne_core::python_env::terminal_env(&self.root);
        vars.extend(env);
        let view = cx.new(|cx| TerminalView::new(cwd, command, vars, path_prepend, cx));
        let sub = cx.subscribe(&view, |this, view, event, cx| match event {
            TerminalViewEvent::OpenFile(path, line, col) => cx.emit(TerminalPanelEvent::OpenFile(path.clone(), *line, *col)),
            TerminalViewEvent::Exited => {
                if let Some(ix) = this.tabs.iter().position(|t| t.view == view) {
                    if !this.tabs[ix].is_run {
                        this.close_tab(ix, cx);
                    }
                }
                cx.notify();
            }
        });
        (view, sub)
    }

    /// The project's Python environment appeared or changed: shell tabs nobody has typed in
    /// yet start again with it (the first one opens with the project, before `.venv` exists).
    /// Returns how many shell tabs were left alone because they are in use.
    pub fn refresh_untouched(&mut self, cx: &mut Context<Self>) -> usize {
        let mut kept = 0;
        for ix in 0..self.tabs.len() {
            if self.tabs[ix].is_run {
                continue;
            }
            let (typed, cwd) = {
                let v = self.tabs[ix].view.read(cx);
                (v.typed, v.cwd.clone())
            };
            if typed {
                kept += 1;
                continue;
            }
            let (view, sub) = self.make_view(cwd, None, Vec::new(), cx);
            let old = std::mem::replace(&mut self.tabs[ix].view, view);
            self.tabs[ix]._sub = sub;
            if let Some(state) = &old.read(cx).state {
                state.kill();
            }
        }
        cx.notify();
        kept
    }

    pub fn new_terminal(&mut self, cx: &mut Context<Self>) -> Entity<TerminalView> {
        self.counter += 1;
        let title: SharedString = if self.counter == 1 { "Terminal".into() } else { format!("Terminal {}", self.counter).into() };
        let root = self.root.clone();
        self.add_tab(title, false, root, cx)
    }

    pub fn ensure_terminal(&mut self, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            self.new_terminal(cx);
        }
    }

    /// One-Click Run: reuse (or create) the tab named after the run configuration.
    pub fn run_in_tab(&mut self, label: &str, cwd: PathBuf, line: &str, cx: &mut Context<Self>) {
        let view = match self.tabs.iter().position(|t| t.is_run && t.title.as_ref() == label && !t.view.read(cx).has_exited()) {
            Some(ix) => {
                self.active = ix;
                self.tabs[ix].view.clone()
            }
            None => self.add_tab(label.to_string().into(), true, cwd, cx),
        };
        view.update(cx, |v, _| v.send_text(line));
        cx.notify();
    }

    /// A debug adapter's `runInTerminal`: run the debuggee directly (no shell) in its own
    /// tab so it can read stdin. A finished tab with the same title is replaced.
    pub fn run_process_in_tab(&mut self, title: &str, cwd: PathBuf, args: Vec<String>, env: Vec<(String, String)>, cx: &mut Context<Self>) {
        let Some((program, rest)) = args.split_first() else { return };
        if let Some(ix) = self.tabs.iter().position(|t| t.is_run && t.title.as_ref() == title && t.view.read(cx).has_exited()) {
            self.close_tab(ix, cx);
        }
        self.add_tab_with(title.to_string().into(), true, cwd, Some((program.clone(), rest.to_vec())), env, cx);
    }

    pub fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() {
            let tab = self.tabs.remove(ix);
            if let Some(state) = &tab.view.read(cx).state {
                state.kill();
            }
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
            cx.notify();
        }
    }

    pub fn focus_active(&self, window: &mut Window, cx: &App) {
        if let Some(t) = self.tabs.get(self.active) {
            window.focus(&t.view.focus_handle(cx));
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.tabs.get(self.active).is_some_and(|t| t.view.focus_handle(cx).is_focused(window));
        let active = self.active;
        let mut header = island_header();
        let debug_title = self.debugger.as_ref().and_then(|d| d.read(cx).session.as_ref().map(|s| s.title.clone()));
        if let Some(t) = debug_title {
            header = header.child(
                div()
                    .id("term-debug-tab")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(theme::UI_TEXT))
                    .text_color(color(c::TEXT_MUTED))
                    .hover(|s| s.bg(color(c::HOVER)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalPanelEvent::ShowDebug)))
                    .child(icon(Icon::Bug, 13., color(c::GREEN)))
                    .child(format!("Debug: {t}")),
            );
        }
        for (ix, tab) in self.tabs.iter().enumerate() {
            let is_active = ix == active;
            let exited = tab.view.read(cx).has_exited();
            let title = tab.title.clone();
            let ic = if tab.is_run { Icon::Play } else { Icon::Terminal };
            header = header.child(
                div()
                    .id(("term-tab", ix))
                    .group("term-tab")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .text_size(px(theme::UI_TEXT))
                    .cursor_pointer()
                    .when(is_active && focused, |d| d.bg(color(c::TAB_FOCUSED)).border_color(color(c::TAB_FOCUSED_BORDER)).text_color(color(c::TEXT)))
                    .when(is_active && !focused, |d| d.bg(color(c::TAB_ACTIVE)).text_color(color(c::TEXT)))
                    .when(!is_active, |d| d.text_color(color(c::TEXT_MUTED)).hover(|s| s.bg(color(c::HOVER))))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = ix;
                        this.focus_active(window, cx);
                        cx.notify();
                    }))
                    .child(icon(ic, 13., if tab.is_run { color(c::GREEN) } else { color(c::TEXT_MUTED) }))
                    .child(if exited { format!("{title} (exited)") } else { title.to_string() })
                    .child(
                        div()
                            .id(("term-close", ix))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .rounded(px(4.))
                            .hover(|s| s.bg(color(c::PRESSED)))
                            .child(icon(Icon::Close, 11., color(c::TEXT_DIM)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, cx);
                            })),
                    ),
            );
        }
        let detached = self.detached;
        header = header
            .child(
                small_icon_button("term-new", Icon::Plus, color(c::TEXT_MUTED))
                    .tooltip(tooltip("New Terminal", Some("Ctrl+Shift+T")))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_terminal(cx);
                        this.focus_active(window, cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("term-detach")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(theme::UI_SMALL))
                    .text_color(color(c::TEXT_MUTED))
                    .hover(|s| s.bg(color(c::HOVER)).text_color(color(c::TEXT)))
                    .tooltip(tooltip(if detached { "Attach back to the main window" } else { "Detach into a separate window" }, Some("Alt+Shift+F12")))
                    .child(icon(if detached { Icon::Attach } else { Icon::Detach }, 13., color(c::TEXT_MUTED)))
                    .child(if detached { "Attach" } else { "Detach" })
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalPanelEvent::ToggleDetach))),
            );

        let body: gpui::AnyElement = match self.tabs.get(self.active) {
            Some(tab) => div().flex_1().min_h(px(0.)).child(tab.view.clone()).into_any_element(),
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(color(c::TEXT_DIM))
                .child("No terminal sessions — press + to start one")
                .into_any_element(),
        };
        island().size_full().font_family(theme::fonts().ui).child(header).child(body)
    }
}

// ---------------------------------------------------------------------------------------

/// Root view of the detached terminal OS window.
pub struct DetachedTerminal {
    pub panel: Entity<TerminalPanel>,
    pub title: SharedString,
    pub workspace: WeakEntity<crate::workspace::Workspace>,
}

impl Render for DetachedTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let maximized = window.is_maximized();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(color(c::CANVAS))
            .font_family(theme::fonts().ui)
            .text_color(color(c::TEXT))
            .child(crate::workspace::caption_bar(self.title.clone(), maximized, cx))
            .child(div().flex_1().min_h(px(0.)).px(theme::gap()).pb(theme::gap()).child(self.panel.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_compiler_and_python_links() {
        let l = find_links(r#"  File "C:\p\a.py", line 31, in <module>"#);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].1.as_str(), l[0].2), (r"C:\p\a.py", 31));
        let l = find_links("error: src/lib.rs:10:5 expected `;`");
        assert_eq!((l[0].1.as_str(), l[0].2, l[0].3), ("src/lib.rs", 10, 5));
        assert!(find_links("listening on 127.0.0.1:8080").is_empty());
        let l = find_links(r"C:\x\main.go:7");
        assert_eq!((l[0].1.as_str(), l[0].2), (r"C:\x\main.go", 7));
    }
}
