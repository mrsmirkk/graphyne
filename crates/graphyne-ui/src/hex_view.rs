//! Native Hex Viewer / in-place binary editor over a memory map.
//! Only visible rows are materialised (uniform_list virtualisation), so multi-GB dumps
//! open instantly.

use crate::theme::{self, c, color};
use gpui::prelude::*;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, SharedString, Window, div, px, uniform_list,
};
use std::path::PathBuf;
use std::sync::Arc;

const BYTES_PER_ROW: usize = 16;

pub struct HexView {
    focus_handle: FocusHandle,
    pub path: PathBuf,
    map: Option<Arc<memmap2::Mmap>>,
    error: Option<String>,
    cursor: usize,
    /// Pending high nibble while typing a byte.
    nibble: Option<u8>,
    status: Option<SharedString>,
}

pub enum HexEvent {}
impl EventEmitter<HexEvent> for HexView {}

impl Focusable for HexView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl HexView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self { focus_handle: cx.focus_handle(), path: path.clone(), map: None, error: None, cursor: 0, nibble: None, status: None };
        view.reload(cx);
        view
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { graphyne_core::fs_util::map_file(&path) }).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(map) => this.map = Some(Arc::new(map)),
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn title(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.map.as_ref().map(|m| m.len()).unwrap_or(0)
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let len = self.len();
        if len == 0 {
            return;
        }
        let k = &ev.keystroke;
        let mv = |c: usize, d: isize| (c as isize + d).clamp(0, len as isize - 1) as usize;
        match k.key.as_str() {
            "left" => self.cursor = mv(self.cursor, -1),
            "right" => self.cursor = mv(self.cursor, 1),
            "up" => self.cursor = mv(self.cursor, -(BYTES_PER_ROW as isize)),
            "down" => self.cursor = mv(self.cursor, BYTES_PER_ROW as isize),
            "pageup" => self.cursor = mv(self.cursor, -(BYTES_PER_ROW as isize) * 32),
            "pagedown" => self.cursor = mv(self.cursor, BYTES_PER_ROW as isize * 32),
            "home" => self.cursor -= self.cursor % BYTES_PER_ROW,
            "end" => self.cursor = (self.cursor - self.cursor % BYTES_PER_ROW + BYTES_PER_ROW - 1).min(len - 1),
            key if key.len() == 1 && key.chars().all(|c| c.is_ascii_hexdigit()) && !k.modifiers.control => {
                let v = u8::from_str_radix(key, 16).unwrap_or(0);
                match self.nibble.take() {
                    None => self.nibble = Some(v),
                    Some(hi) => {
                        let byte = (hi << 4) | v;
                        let offset = self.cursor as u64;
                        let path = self.path.clone();
                        self.status = Some(format!("Writing 0x{byte:02X} at 0x{offset:08X}…").into());
                        cx.spawn(async move |this, cx| {
                            let res = cx.background_spawn(async move { graphyne_core::fs_util::patch_bytes(&path, offset, &[byte]) }).await;
                            this.update(cx, |this, cx| {
                                this.status = Some(match res {
                                    Ok(()) => format!("Patched 0x{offset:08X} = 0x{byte:02X}").into(),
                                    Err(e) => format!("Write failed: {e}").into(),
                                });
                                this.reload(cx);
                            })
                            .ok();
                        })
                        .detach();
                        self.cursor = mv(self.cursor, 1);
                    }
                }
            }
            _ => return,
        }
        cx.notify();
    }
}

impl Render for HexView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let len = self.len();
        let rows = len.div_ceil(BYTES_PER_ROW);
        let map = self.map.clone();
        let cursor = self.cursor;
        let nibble = self.nibble;
        let header = div()
            .flex()
            .flex_none()
            .h(px(28.))
            .items_center()
            .px(px(16.))
            .gap(px(24.))
            .text_color(color(c::TEXT_DIM))
            .border_b_1()
            .border_color(color(c::BORDER))
            .child(div().w(px(84.)).child("Offset"))
            .child(div().w(px(16. * 24.)).child("00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F"))
            .child("ASCII")
            .child(div().flex_1())
            .child(format!("{len} bytes"))
            .children(self.status.clone());
        let entity = cx.entity();
        div()
            .id("hex")
            .key_context("HexView")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, _| window.focus(&this.focus_handle)))
            .flex()
            .flex_col()
            .size_full()
            .font_family(theme::fonts().code)
            .text_size(px(13.))
            .child(header)
            .when_some(self.error.clone(), |d, err| d.child(div().p(px(16.)).text_color(color(c::RED)).child(err)))
            .child(
                uniform_list("hex-rows", rows, move |range, _window, _cx| {
                    let Some(map) = map.as_ref() else { return Vec::new() };
                    range
                        .map(|row| {
                            let start = row * BYTES_PER_ROW;
                            let end = (start + BYTES_PER_ROW).min(map.len());
                            let bytes = &map[start..end];
                            let entity = entity.clone();
                            let mut hex = div().flex().w(px(16. * 24.));
                            for (i, b) in bytes.iter().enumerate() {
                                let off = start + i;
                                let is_cur = off == cursor;
                                let text = if is_cur && nibble.is_some() { format!("{:X}_", nibble.unwrap_or(0)) } else { format!("{b:02X}") };
                                let ent = entity.clone();
                                hex = hex.child(
                                    div()
                                        .id(off)
                                        .w(px(22.))
                                        .when(i == 8, |d| d.ml(px(8.)))
                                        .text_color(if *b == 0 { color(c::TEXT_FAINT) } else { color(c::TEXT) })
                                        .when(is_cur, |d| d.bg(color(c::SELECTION)).rounded(px(3.)))
                                        .cursor_pointer()
                                        .on_click(move |_, _, cx| {
                                            ent.update(cx, |h, cx| {
                                                h.cursor = off;
                                                h.nibble = None;
                                                cx.notify();
                                            })
                                        })
                                        .child(text),
                                );
                            }
                            let ascii: String = bytes.iter().map(|b| if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '·' }).collect();
                            div()
                                .flex()
                                .h(px(22.))
                                .items_center()
                                .px(px(16.))
                                .gap(px(24.))
                                .when(cursor / BYTES_PER_ROW == row, |d| d.bg(color(c::ACTIVE_LINE)))
                                .child(div().w(px(84.)).text_color(color(c::TEXT_DIM)).child(format!("{start:08X}")))
                                .child(hex)
                                .child(div().text_color(color(0xA8CC7C)).child(ascii))
                        })
                        .collect()
                })
                .flex_1(),
            )
    }
}
