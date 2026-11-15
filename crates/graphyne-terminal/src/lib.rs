//! Graphyne terminal engine.
//!
//! [`TerminalState`] owns the PTY (`portable-pty`) and the VT state machine
//! (`alacritty_terminal`). It knows nothing about views: the same state can be rendered
//! in the bottom island or in a detached OS window and moved between them freely.
//!
//! PTY reading happens on a dedicated thread. After each chunk is parsed the thread
//! pings a bounded(1) channel, so a gigabyte of `nmap` output produces at most one
//! pending repaint request at a time — the UI thread never falls behind.

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor, Rgb as AnsiRgb};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }
    pub fn to_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }
    fn dim(self) -> Self {
        Rgb { r: (self.r as u16 * 2 / 3) as u8, g: (self.g as u16 * 2 / 3) as u8, b: (self.b as u16 * 2 / 3) as u8 }
    }
}

/// Terminal colour scheme ("Fleet Dark" flavoured).
#[derive(Clone, Debug)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            foreground: Rgb::hex(0xD1D3D9),
            background: Rgb::hex(0x191B1E),
            cursor: Rgb::hex(0xD1D3D9),
            ansi: [
                Rgb::hex(0x3B3E44),
                Rgb::hex(0xEC7B7B),
                Rgb::hex(0xA8CC7C),
                Rgb::hex(0xEBC88D),
                Rgb::hex(0x87C3FF),
                Rgb::hex(0xE394DC),
                Rgb::hex(0x83D6C5),
                Rgb::hex(0xC8CBD0),
                Rgb::hex(0x6B6F77),
                Rgb::hex(0xF2A19D),
                Rgb::hex(0xBEE09A),
                Rgb::hex(0xF3DAAE),
                Rgb::hex(0xA9D3FF),
                Rgb::hex(0xEDB3E8),
                Rgb::hex(0xA3E4D8),
                Rgb::hex(0xEDEEF0),
            ],
        }
    }
}

impl Palette {
    fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb { r: step(i / 36), g: step((i / 6) % 6), b: step(i % 6) }
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                Rgb { r: v, g: v, b: v }
            }
        }
    }

    fn named(&self, n: NamedColor) -> Rgb {
        let ix = n as usize;
        if ix < 16 {
            return self.ansi[ix];
        }
        match n {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::DimForeground => self.foreground.dim(),
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            other => {
                let base = other as usize - NamedColor::DimBlack as usize;
                self.ansi[base.min(7)].dim()
            }
        }
    }

    fn resolve(&self, color: AnsiColor, overrides: &Colors) -> Rgb {
        let conv = |c: AnsiRgb| Rgb { r: c.r, g: c.g, b: c.b };
        match color {
            AnsiColor::Spec(c) => conv(c),
            AnsiColor::Named(n) => overrides[n].map(conv).unwrap_or_else(|| self.named(n)),
            AnsiColor::Indexed(i) => overrides[i as usize].map(conv).unwrap_or_else(|| self.indexed(i)),
        }
    }
}

#[derive(Clone)]
struct Listener {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    title: Arc<Mutex<Option<String>>>,
    size: Arc<Mutex<WindowSize>>,
    palette: Arc<Palette>,
}

impl EventListener for Listener {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(text) => {
                let mut w = self.writer.lock();
                let _ = w.write_all(text.as_bytes());
                let _ = w.flush();
            }
            AlacEvent::Title(t) => *self.title.lock() = Some(t),
            AlacEvent::ResetTitle => *self.title.lock() = None,
            AlacEvent::TextAreaSizeRequest(format) => {
                let reply = format(*self.size.lock());
                let _ = self.writer.lock().write_all(reply.as_bytes());
            }
            AlacEvent::ColorRequest(ix, format) => {
                let c = if ix < 16 {
                    self.palette.ansi[ix]
                } else if ix == NamedColor::Background as usize {
                    self.palette.background
                } else {
                    self.palette.foreground
                };
                let reply = format(AnsiRgb { r: c.r, g: c.g, b: c.b });
                let _ = self.writer.lock().write_all(reply.as_bytes());
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpawnOptions {
    pub cwd: PathBuf,
    /// Program + args. `None` = the user's default shell.
    pub command: Option<(String, Vec<String>)>,
    /// Extra environment for the child (e.g. a debug adapter's `runInTerminal` env).
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// Shell chosen in Settings ("auto" or an id from [`available_shells`]).
static PREFERRED_SHELL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Use this shell for new terminal tabs (`None` / "auto": the platform default).
pub fn set_preferred_shell(id: Option<String>) {
    if let Ok(mut s) = PREFERRED_SHELL.lock() {
        *s = id.filter(|id| id != "auto");
    }
}

#[cfg(windows)]
fn git_bash() -> Option<std::path::PathBuf> {
    ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"].iter().filter_map(std::env::var_os).find_map(|base| {
        let base = std::path::PathBuf::from(base);
        [base.join("Git").join("bin").join("bash.exe"), base.join("Programs").join("Git").join("bin").join("bash.exe")].into_iter().find(|p| p.is_file())
    })
}

fn in_path(exe: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file())).unwrap_or(false)
}

/// Shells found on this machine, as (id, label); "auto" first.
pub fn available_shells() -> Vec<(String, String)> {
    let mut out = vec![("auto".to_string(), "Automatic".to_string())];
    #[cfg(windows)]
    {
        if in_path("pwsh.exe") {
            out.push(("pwsh".into(), "PowerShell 7".into()));
        }
        out.push(("powershell".into(), "Windows PowerShell".into()));
        out.push(("cmd".into(), "Command Prompt".into()));
        if git_bash().is_some() {
            out.push(("gitbash".into(), "Git Bash".into()));
        }
        if in_path("wsl.exe") {
            out.push(("wsl".into(), "WSL".into()));
        }
    }
    #[cfg(not(windows))]
    for (id, label) in [("bash", "bash"), ("zsh", "zsh"), ("fish", "fish")] {
        if in_path(id) {
            out.push((id.into(), label.into()));
        }
    }
    out
}

fn shell_command(id: &str) -> Option<(String, Vec<String>)> {
    #[cfg(windows)]
    {
        match id {
            "pwsh" => Some(("pwsh.exe".into(), vec!["-NoLogo".into()])),
            "powershell" => Some(("powershell.exe".into(), vec!["-NoLogo".into()])),
            "cmd" => Some(("cmd.exe".into(), Vec::new())),
            "gitbash" => git_bash().map(|p| (p.to_string_lossy().into_owned(), vec!["--login".into(), "-i".into()])),
            "wsl" => Some(("wsl.exe".into(), Vec::new())),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        matches!(id, "bash" | "zsh" | "fish").then(|| (id.to_string(), vec!["-l".into()]))
    }
}

/// The interactive shell for new terminal tabs: the one chosen in Settings, else the
/// platform default.
pub fn default_shell() -> (String, Vec<String>) {
    let preferred = PREFERRED_SHELL.lock().ok().and_then(|s| s.clone());
    if let Some(cmd) = preferred.as_deref().and_then(shell_command) {
        return cmd;
    }
    #[cfg(windows)]
    {
        if in_path("pwsh.exe") {
            return ("pwsh.exe".into(), vec!["-NoLogo".into()]);
        }
        ("powershell.exe".into(), vec!["-NoLogo".into()])
    }
    #[cfg(not(windows))]
    {
        (std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()), vec!["-l".into()])
    }
}

/// One styled run of cells in a row, positioned by column (keeps the grid aligned).
#[derive(Clone, Debug, PartialEq)]
pub struct TermRun {
    pub col: usize,
    pub cells: usize,
    pub text: String,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TerminalSnapshot {
    pub rows: Vec<Vec<TermRun>>,
    /// Plain text of each visible row (link detection, copy).
    pub row_text: Vec<String>,
    pub cursor: Option<(usize, usize)>,
    pub cursor_block: bool,
    pub cols: usize,
    pub screen_lines: usize,
    pub display_offset: usize,
    pub history_size: usize,
    pub selection: Option<((usize, usize), (usize, usize))>,
}

pub struct TerminalState {
    term: Arc<FairMutex<Term<Listener>>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    title: Arc<Mutex<Option<String>>>,
    size: Arc<Mutex<WindowSize>>,
    exited: Arc<AtomicBool>,
    palette: Arc<Palette>,
    wakeups: flume::Receiver<()>,
    shell_name: String,
}

impl TerminalState {
    pub fn spawn(opts: SpawnOptions) -> Result<Self> {
        let cols = opts.cols.max(10);
        let rows = opts.rows.max(3);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("opening pty")?;
        let (program, args) = opts.command.unwrap_or_else(default_shell);
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&args);
        cmd.cwd(&opts.cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Graphyne");
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("spawning {program}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("cloning pty reader")?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer().context("taking pty writer")?));
        let title = Arc::new(Mutex::new(None));
        let size = Arc::new(Mutex::new(WindowSize { num_lines: rows, num_cols: cols, cell_width: 8, cell_height: 16 }));
        let palette = Arc::new(Palette::default());
        let listener = Listener { writer: writer.clone(), title: title.clone(), size: size.clone(), palette: palette.clone() };

        let config = TermConfig { scrolling_history: 10_000, ..TermConfig::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &TermSize::new(cols as usize, rows as usize), listener)));

        let (tx, rx) = flume::bounded::<()>(1);
        let exited = Arc::new(AtomicBool::new(false));
        {
            let term = term.clone();
            let exited = exited.clone();
            std::thread::Builder::new()
                .name("graphyne-pty-reader".into())
                .spawn(move || {
                    let mut processor: Processor = Processor::new();
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                {
                                    let mut t = term.lock();
                                    processor.advance(&mut *t, &buf[..n]);
                                }
                                let _ = tx.try_send(());
                            }
                        }
                    }
                    exited.store(true, Ordering::SeqCst);
                    let _ = tx.try_send(());
                })
                .context("spawning pty reader thread")?;
        }

        let shell_name = std::path::Path::new(&program)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(program.clone());

        Ok(Self {
            term,
            master: Mutex::new(pair.master),
            writer,
            child: Mutex::new(child),
            title,
            size,
            exited,
            palette,
            wakeups: rx,
            shell_name,
        })
    }

    /// Receiver that yields whenever new output has been parsed (coalesced).
    pub fn wakeups(&self) -> flume::Receiver<()> {
        self.wakeups.clone()
    }

    pub fn title(&self) -> Option<String> {
        self.title.lock().clone()
    }
    pub fn shell_name(&self) -> &str {
        &self.shell_name
    }
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn write(&self, bytes: &[u8]) {
        if self.has_exited() {
            return;
        }
        let mut w = self.writer.lock();
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    /// Typing snaps the viewport back to the live screen.
    pub fn input(&self, bytes: &[u8]) {
        {
            let mut t = self.term.lock();
            if t.grid().display_offset() != 0 {
                t.scroll_display(Scroll::Bottom);
            }
            t.selection = None;
        }
        self.write(bytes);
    }

    pub fn paste(&self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if bracketed {
            self.input(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.input(text.as_bytes());
        }
    }

    pub fn app_cursor(&self) -> bool {
        self.term.lock().mode().contains(TermMode::APP_CURSOR)
    }

    pub fn size(&self) -> (u16, u16) {
        let s = self.size.lock();
        (s.num_cols, s.num_lines)
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        let cols = cols.max(10);
        let rows = rows.max(2);
        {
            let mut s = self.size.lock();
            if s.num_cols == cols && s.num_lines == rows {
                return;
            }
            *s = WindowSize { num_lines: rows, num_cols: cols, cell_width, cell_height };
        }
        let _ = self.master.lock().resize(PtySize { rows, cols, pixel_width: cols * cell_width, pixel_height: rows * cell_height });
        self.term.lock().resize(TermSize::new(cols as usize, rows as usize));
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// Start a selection at a viewport cell.
    pub fn start_selection(&self, row: usize, col: usize, kind: SelectionKind) {
        let mut t = self.term.lock();
        let point = viewport_to_grid(&t, row, col);
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        t.selection = Some(Selection::new(ty, point, Side::Left));
    }

    pub fn update_selection(&self, row: usize, col: usize) {
        let mut t = self.term.lock();
        let point = viewport_to_grid(&t, row, col);
        if let Some(sel) = t.selection.as_mut() {
            sel.update(point, Side::Right);
        }
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let cols = term.columns();
        let screen_lines = term.screen_lines();
        let display_offset = content.display_offset;
        let mut rows: Vec<Vec<TermRun>> = vec![Vec::new(); screen_lines];
        let mut row_text: Vec<String> = vec![String::new(); screen_lines];
        let palette = &*self.palette;
        let default_bg = palette.background;

        for indexed in content.display_iter {
            let line = indexed.point.line.0 + display_offset as i32;
            if line < 0 || line as usize >= screen_lines {
                continue;
            }
            let row = line as usize;
            let col = indexed.point.column.0;
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let mut fg = palette.resolve(cell.fg, content.colors);
            let mut bg = palette.resolve(cell.bg, content.colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.dim();
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let bg = if bg == default_bg { None } else { Some(bg) };
            let mut ch = cell.c;
            if ch == '\0' || ch == '\t' {
                ch = ' ';
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let bold = cell.flags.contains(Flags::BOLD);
            let italic = cell.flags.contains(Flags::ITALIC);
            let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
            let strike = cell.flags.contains(Flags::STRIKEOUT);

            row_text[row].push(ch);
            let runs = &mut rows[row];
            if let Some(last) = runs.last_mut() {
                if last.col + last.cells == col
                    && last.fg == fg
                    && last.bg == bg
                    && last.bold == bold
                    && last.italic == italic
                    && last.underline == underline
                    && last.strike == strike
                    && width == 1
                    && !last.text.chars().last().map(is_wide).unwrap_or(false)
                {
                    last.text.push(ch);
                    if let Some(zw) = cell.zerowidth() {
                        last.text.extend(zw.iter());
                    }
                    last.cells += 1;
                    continue;
                }
            }
            let mut text = String::new();
            text.push(ch);
            if let Some(zw) = cell.zerowidth() {
                text.extend(zw.iter());
            }
            runs.push(TermRun { col, cells: width, text, fg, bg, bold, italic, underline, strike });
        }

        // Drop trailing blank runs without background (cheaper shaping).
        for runs in rows.iter_mut() {
            while runs.last().map(|r| r.bg.is_none() && r.text.trim().is_empty()).unwrap_or(false) {
                runs.pop();
            }
        }

        let cursor_visible = content.cursor.shape != CursorShape::Hidden && display_offset == 0;
        let cursor = if cursor_visible {
            let line = content.cursor.point.line.0 + display_offset as i32;
            (line >= 0 && (line as usize) < screen_lines).then(|| (line as usize, content.cursor.point.column.0))
        } else {
            None
        };
        let cursor_block = matches!(content.cursor.shape, CursorShape::Block | CursorShape::HollowBlock);
        let selection = content.selection.map(|s| {
            let to_view = |p: GridPoint| {
                let l = (p.line.0 + display_offset as i32).clamp(0, screen_lines as i32 - 1) as usize;
                (l, p.column.0)
            };
            (to_view(s.start), to_view(s.end))
        });
        let history_size = term.grid().history_size();
        TerminalSnapshot { rows, row_text, cursor, cursor_block, cols, screen_lines, display_offset, history_size, selection }
    }

    pub fn kill(&self) {
        let _ = self.child.lock().kill();
    }
}

impl Drop for TerminalState {
    fn drop(&mut self) {
        self.kill();
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
}

fn viewport_to_grid<T>(term: &Term<T>, row: usize, col: usize) -> GridPoint {
    let offset = term.grid().display_offset() as i32;
    let line = (row as i32 - offset).min(term.screen_lines() as i32 - 1);
    GridPoint::new(Line(line), Column(col.min(term.columns().saturating_sub(1))))
}

fn is_wide(c: char) -> bool {
    (c as u32) >= 0x1100
        && matches!(c as u32,
            0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF)
}

/// A key press, in GPUI's vocabulary (`key` like `"enter"`, `"a"`, `"up"`).
#[derive(Clone, Copy, Debug)]
pub struct KeyInput<'a> {
    pub key: &'a str,
    pub key_char: Option<&'a str>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Translate a key press into the bytes a VT/xterm application expects.
pub fn key_to_bytes(k: KeyInput, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier_param = 1 + (k.shift as u8) + 2 * (k.alt as u8) + 4 * (k.ctrl as u8);
    let csi_mod = |final_byte: char| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[1;{modifier_param}{final_byte}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[{code};{modifier_param}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    let named: Option<Vec<u8>> = match k.key {
        "enter" => Some(if k.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }),
        "backspace" => Some(if k.ctrl { vec![0x08] } else if k.alt { b"\x1b\x7f".to_vec() } else { vec![0x7f] }),
        "tab" => Some(if k.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
        "escape" => Some(vec![0x1b]),
        "up" => Some(csi_mod('A')),
        "down" => Some(csi_mod('B')),
        "right" => Some(csi_mod('C')),
        "left" => Some(csi_mod('D')),
        "home" => Some(csi_mod('H')),
        "end" => Some(csi_mod('F')),
        "insert" => Some(tilde(2)),
        "delete" => Some(tilde(3)),
        "pageup" => Some(tilde(5)),
        "pagedown" => Some(tilde(6)),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(tilde(15)),
        "f6" => Some(tilde(17)),
        "f7" => Some(tilde(18)),
        "f8" => Some(tilde(19)),
        "f9" => Some(tilde(20)),
        "f10" => Some(tilde(21)),
        "f11" => Some(tilde(23)),
        "f12" => Some(tilde(24)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if k.ctrl && !k.alt {
        let key = k.key.as_bytes();
        if key.len() == 1 {
            let c = key[0].to_ascii_lowercase();
            let byte = match c {
                b'a'..=b'z' => Some(c - b'a' + 1),
                b'@' | b'2' | b' ' => Some(0),
                b'[' | b'3' => Some(0x1b),
                b'\\' | b'4' => Some(0x1c),
                b']' | b'5' => Some(0x1d),
                b'^' | b'6' => Some(0x1e),
                b'_' | b'-' | b'7' => Some(0x1f),
                b'8' => Some(0x7f),
                _ => None,
            };
            if let Some(b) = byte {
                return Some(vec![b]);
            }
        }
        if k.key == "space" {
            return Some(vec![0]);
        }
    }
    let text = match (k.key_char, k.key) {
        (Some(c), _) => c.to_string(),
        (None, "space") => " ".to_string(),
        _ => return None,
    };
    if k.alt {
        let mut v = vec![0x1b];
        v.extend_from_slice(text.as_bytes());
        Some(v)
    } else {
        Some(text.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: &str, ch: Option<&str>) -> KeyInput<'static> {
        let key: &'static str = Box::leak(key.to_string().into_boxed_str());
        let ch: Option<&'static str> = ch.map(|c| &*Box::leak(c.to_string().into_boxed_str()));
        KeyInput { key, key_char: ch, ctrl: false, alt: false, shift: false }
    }

    #[test]
    fn keys() {
        assert_eq!(key_to_bytes(k("enter", None), false), Some(b"\r".to_vec()));
        assert_eq!(key_to_bytes(k("up", None), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_to_bytes(k("up", None), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_to_bytes(KeyInput { ctrl: true, ..k("c", None) }, false), Some(vec![3]));
        assert_eq!(key_to_bytes(k("a", Some("a")), false), Some(b"a".to_vec()));
        assert_eq!(key_to_bytes(KeyInput { ctrl: true, ..k("right", None) }, false), Some(b"\x1b[1;5C".to_vec()));
    }

    #[test]
    fn palette_cube() {
        let p = Palette::default();
        assert_eq!(p.indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(p.indexed(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(p.indexed(255), Rgb { r: 238, g: 238, b: 238 });
    }
}
