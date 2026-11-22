//! Debug island (bottom panel): toolbar, frames, variables tree, console + evaluate.

use crate::assets::Icon;
use crate::components::{icon, island, island_header, tooltip};
use crate::debugger::{Debugger, Status};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{self, c, color};
use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, ScrollHandle, SharedString, Subscription, Window, div, px};

pub enum DebugPanelEvent {
    ShowTerminal,
}

pub struct DebugPanel {
    debugger: Entity<Debugger>,
    eval: Entity<TextInput>,
    console_scroll: ScrollHandle,
    last_console_len: usize,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DebugPanelEvent> for DebugPanel {}

impl DebugPanel {
    pub fn new(debugger: Entity<Debugger>, cx: &mut Context<Self>) -> Self {
        let eval = cx.new(|cx| TextInput::new("Evaluate expression in the selected frame…", cx));
        let mut subs = vec![cx.observe(&debugger, |_, _, cx| cx.notify())];
        let dbg = debugger.clone();
        subs.push(cx.subscribe(&eval, move |_, input, ev, cx| {
            if let TextInputEvent::Submit = ev {
                let expr = input.read(cx).text().trim().to_string();
                if !expr.is_empty() {
                    dbg.update(cx, |d, cx| d.evaluate(expr, cx));
                    input.update(cx, |i, cx| i.set_text("", cx));
                }
            }
        }));
        Self { debugger, eval, console_scroll: ScrollHandle::new(), last_console_len: 0, _subs: subs }
    }

    fn tool(&self, id: &'static str, ic: Icon, tip: &'static str, key: Option<&'static str>, enabled: bool, tint: u32, f: fn(&mut Debugger, &mut Context<Debugger>), _cx: &mut Context<Self>) -> impl IntoElement {
        let debugger = self.debugger.clone();
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded(px(6.))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(color(c::HOVER))).on_click(move |_, _, cx| debugger.update(cx, |d, cx| f(d, cx))))
            .tooltip(tooltip(tip, key))
            .child(icon(ic, 15., if enabled { color(tint) } else { color(c::TEXT_FAINT) }))
            .into_any_element()
            .into_element()
    }
}

fn column_title(t: &'static str) -> impl IntoElement {
    div().flex_none().h(px(26.)).px(px(10.)).flex().items_center().text_size(px(11.)).text_color(color(c::TEXT_DIM)).child(t)
}

impl Render for DebugPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.debugger.read(cx);
        let Some(s) = d.session.as_ref() else {
            return island().size_full().child(div().p(px(16.)).text_color(color(c::TEXT_DIM)).child("No debug session")).into_any_element();
        };
        let paused = matches!(s.status, Status::Paused { .. });
        let running = matches!(s.status, Status::Running);
        let live = s.status.is_live();
        let (status_text, status_color) = match &s.status {
            Status::Starting(m) => (m.clone(), c::YELLOW),
            Status::Running => ("Running".to_string(), c::GREEN),
            Status::Paused { reason, description } => (
                // js-debug says "Paused on breakpoint" itself; debugpy only gives a reason.
                match description {
                    Some(d) if d.to_lowercase().starts_with("paused") => d.clone(),
                    Some(d) => format!("Paused: {d}"),
                    None => format!("Paused: {reason}"),
                },
                c::YELLOW,
            ),
            Status::Exited(code) => (format!("Exited with code {code}"), c::TEXT_DIM),
            Status::Stopped => ("Stopped".to_string(), c::TEXT_DIM),
            Status::Failed(e) => (format!("Failed: {e}"), c::RED),
        };
        let title: SharedString = format!("Debug: {}", s.title).into();
        let adapter = s.adapter.clone();
        let frames = s.frames.clone();
        let selected = s.selected_frame;
        let console = s.console.clone();
        let rows = d.variable_rows();

        if console.len() != self.last_console_len {
            self.last_console_len = console.len();
            if !console.is_empty() {
                self.console_scroll.scroll_to_item(console.len() - 1);
            }
        }

        let header = island_header()
            .child(
                div()
                    .id("dbg-terminal-tab")
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
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugPanelEvent::ShowTerminal)))
                    .child(icon(Icon::Terminal, 13., color(c::TEXT_MUTED)))
                    .child("Terminal"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(color(c::TAB_FOCUSED_BORDER))
                    .bg(color(c::TAB_FOCUSED))
                    .text_size(px(theme::UI_TEXT))
                    .text_color(color(c::TEXT))
                    .child(icon(Icon::Bug, 13., color(c::GREEN)))
                    .child(title),
            )
            .child(div().ml(px(10.)).text_size(px(theme::UI_SMALL)).text_color(color(status_color)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(status_text))
            .when(!adapter.is_empty(), |h| h.child(div().ml(px(8.)).text_size(px(11.)).text_color(color(c::TEXT_FAINT)).child(adapter)))
            .child(div().flex_1())
            .child(if running {
                self.tool("dbg-pause", Icon::Pause, "Pause", None, true, c::TEXT, Debugger::pause, cx)
            } else {
                self.tool("dbg-resume", Icon::Resume, "Resume Program", Some("F9"), paused, c::GREEN, Debugger::resume, cx)
            })
            .child(self.tool("dbg-over", Icon::StepOver, "Step Over", Some("F8"), paused, c::ACCENT, Debugger::step_over, cx))
            .child(self.tool("dbg-into", Icon::StepInto, "Step Into", Some("F7"), paused, c::ACCENT, Debugger::step_into, cx))
            .child(self.tool("dbg-out", Icon::StepOut, "Step Out", Some("Shift+F8"), paused, c::ACCENT, Debugger::step_out, cx))
            .child(self.tool("dbg-restart", Icon::Restart, "Restart", None, true, c::TEXT_MUTED, Debugger::restart, cx))
            .child(self.tool("dbg-stop", Icon::Stop, "Stop", Some("Ctrl+F2"), live, c::RED, Debugger::stop, cx));

        let dbg = self.debugger.clone();
        let frames_col = div()
            .flex()
            .flex_col()
            .w(px(260.))
            .flex_none()
            .border_r_1()
            .border_color(color(c::BORDER))
            .child(column_title("FRAMES"))
            .child(
                div()
                    .id("dbg-frames")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(4.))
                    .when(frames.is_empty(), |d| d.child(div().px(px(8.)).text_size(px(12.)).text_color(color(c::TEXT_FAINT)).child(if paused { "Loading…" } else { "Frames appear when the program is paused" })))
                    .children(frames.into_iter().enumerate().map(|(ix, f)| {
                        let dbg = dbg.clone();
                        let loc = format!(
                            "{}:{}",
                            f.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "<unknown>".into()),
                            f.line + 1
                        );
                        div()
                            .id(("frame", ix))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(24.))
                            .px(px(6.))
                            .rounded(px(5.))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .when(ix == selected, |d| d.bg(color(c::TAB_ACTIVE)))
                            .hover(|s| s.bg(color(c::HOVER)))
                            .on_click(move |_, _, cx| dbg.update(cx, |d, cx| d.select_frame(ix, cx)))
                            .child(div().text_color(color(0xEFB080)).whitespace_nowrap().child(f.name))
                            .child(div().flex_1().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(color(c::TEXT_DIM)).child(loc))
                    })),
            );

        let dbg = self.debugger.clone();
        let vars_col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .border_r_1()
            .border_color(color(c::BORDER))
            .child(column_title("VARIABLES"))
            .child(
                div()
                    .id("dbg-vars")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(4.))
                    .font_family(theme::fonts().code)
                    .text_size(px(12.5))
                    .children(rows.into_iter().enumerate().map(|(ix, r)| {
                        let dbg = dbg.clone();
                        let reference = r.reference;
                        let chevron = if r.reference > 0 {
                            icon(if r.expanded { Icon::ChevronDown } else { Icon::ChevronRight }, 11., color(c::TEXT_DIM)).into_any_element()
                        } else {
                            div().w(px(11.)).into_any_element()
                        };
                        div()
                            .id(("var", ix))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .h(px(22.))
                            .pl(px(6. + r.depth as f32 * 14.))
                            .pr(px(6.))
                            .rounded(px(4.))
                            .hover(|s| s.bg(color(c::HOVER)))
                            .when(reference > 0, |d| d.cursor_pointer().on_click(move |_, _, cx| dbg.update(cx, |d, cx| d.toggle_variable(reference, cx))))
                            .child(chevron)
                            .when(r.is_scope, |d| d.child(div().font_family(theme::fonts().ui).text_color(color(c::TEXT_MUTED)).child(r.name.clone())))
                            .when(!r.is_scope, |d| {
                                d.child(div().text_color(color(0xAA9BF5)).whitespace_nowrap().child(r.name.clone()))
                                    .child(div().text_color(color(c::TEXT_DIM)).child("="))
                                    .child(div().flex_1().min_w(px(0.)).whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(color(c::TEXT)).child(r.value.clone()))
                                    .children(r.type_name.clone().map(|t| div().text_color(color(c::TEXT_FAINT)).text_size(px(11.5)).child(t)))
                            })
                    })),
            );

        let console_col = div()
            .flex()
            .flex_col()
            .w(px(380.))
            .flex_none()
            .child(column_title("CONSOLE"))
            .child(
                div()
                    .id("dbg-console")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.console_scroll)
                    .px(px(10.))
                    .font_family(theme::fonts().code)
                    .text_size(px(12.5))
                    .children(console.into_iter().flat_map(|(cat, text)| {
                        let tint = match cat.as_ref() {
                            "stderr" | "error" => c::RED,
                            "input" => c::ACCENT,
                            "result" => c::GREEN,
                            "system" | "console" => c::TEXT_DIM,
                            _ => c::TEXT,
                        };
                        text.trim_end_matches(['\r', '\n'])
                            .split('\n')
                            .map(|l| div().min_h(px(17.)).text_color(color(tint)).child(l.trim_end_matches('\r').to_string()))
                            .collect::<Vec<_>>()
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(30.))
                    .mx(px(6.))
                    .mb(px(6.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .bg(color(c::CANVAS))
                    .border_1()
                    .border_color(color(c::BORDER))
                    .child(div().text_color(color(c::ACCENT)).font_family(theme::fonts().code).child(">"))
                    .child(self.eval.clone()),
            );

        island()
            .size_full()
            .font_family(theme::fonts().ui)
            .child(header)
            .child(div().flex().flex_1().min_h(px(0.)).child(frames_col).child(vars_col).child(console_col))
            .into_any_element()
    }
}
