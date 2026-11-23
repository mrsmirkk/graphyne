//! New / Edit Connection: a dialog for PostgreSQL and MySQL / MariaDB servers, with Test,
//! optionally through an SSH tunnel. Passwords are kept in the Windows Credential Manager,
//! never in Graphyne's files.

use super::DbStore;
use crate::components::tooltip;
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{self, c, color, color_a};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, PathPromptOptions, SharedString, Subscription, Window, div, px};
use graphyne_db::{DbConfig, DbKind, DbSession, ServerConfig, SshConfig, SslMode};

pub enum FormEvent {
    /// Saved; the connection's id.
    Saved(String),
    Closed,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Engine {
    Postgres,
    Mysql,
}

impl Engine {
    fn port(self) -> u16 {
        DbKind::default_port(self == Engine::Mysql)
    }
    fn user(self) -> &'static str {
        if self == Engine::Mysql { "root" } else { "postgres" }
    }
    /// What an empty Database field means.
    fn database_hint(self) -> &'static str {
        if self == Engine::Mysql { "empty: all databases" } else { "empty: the user's own database" }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Info,
    Ok,
    Error,
}

pub struct ConnectionForm {
    focus_handle: FocusHandle,
    store: Entity<DbStore>,
    /// The connection being edited; `None` for a new one.
    editing: Option<String>,
    engine: Engine,
    name: Entity<TextInput>,
    host: Entity<TextInput>,
    port: Entity<TextInput>,
    database: Entity<TextInput>,
    user: Entity<TextInput>,
    password: Entity<TextInput>,
    ssl: SslMode,
    /// Connect through an SSH tunnel (the fields below).
    ssh: bool,
    ssh_host: Entity<TextInput>,
    ssh_port: Entity<TextInput>,
    ssh_user: Entity<TextInput>,
    ssh_key: Entity<TextInput>,
    /// The SSH password, or the key's passphrase.
    ssh_password: Entity<TextInput>,
    status: Option<(SharedString, Tone)>,
    testing: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<FormEvent> for ConnectionForm {}

impl Focusable for ConnectionForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ConnectionForm {
    pub fn new(store: Entity<DbStore>, editing: Option<String>, engine: Engine, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &str, cx: &mut Context<Self>| cx.new(|cx| TextInput::new(placeholder.to_string(), cx));
        let (name, host, port, database, user, password) = (
            input("e.g. Shop (prod)", cx),
            input("localhost", cx),
            input("", cx),
            input("", cx),
            input("", cx),
            input("none", cx),
        );
        password.update(cx, |p, cx| p.set_masked(true, cx));
        let (ssh_host, ssh_port, ssh_user, ssh_key, ssh_password) =
            (input("bastion.example.com", cx), input("22", cx), input("from ~/.ssh/config, else your Windows user", cx), input("your usual keys and ssh-agent", cx), input("none, or the key's passphrase", cx));
        ssh_password.update(cx, |p, cx| p.set_masked(true, cx));
        let existing = editing.as_ref().and_then(|id| store.read(cx).entry(id)).map(|e| e.config.clone());
        let (engine, server, title) = match existing.as_ref().map(|c| (&c.kind, c.name.clone())) {
            Some((DbKind::Postgres(s), title)) => (Engine::Postgres, s.clone(), title),
            Some((DbKind::Mysql(s), title)) => (Engine::Mysql, s.clone(), title),
            _ => (engine, ServerConfig { host: "localhost".into(), port: engine.port(), user: engine.user().into(), ..Default::default() }, String::new()),
        };
        let set = |field: &Entity<TextInput>, text: String, cx: &mut Context<Self>| field.update(cx, |f, cx| f.set_text(text, cx));
        set(&name, title, cx);
        set(&host, server.host.clone(), cx);
        set(&port, server.port.to_string(), cx);
        set(&database, server.database.clone(), cx);
        set(&user, server.user.clone(), cx);
        database.update(cx, |f, cx| f.set_placeholder(engine.database_hint(), cx));
        let tunnel = server.ssh.clone().unwrap_or_default();
        set(&ssh_host, tunnel.host.clone(), cx);
        set(&ssh_port, tunnel.port.to_string(), cx);
        set(&ssh_user, tunnel.user.clone(), cx);
        set(&ssh_key, tunnel.key_file.clone().unwrap_or_default(), cx);
        // The saved passwords are read back so Save keeps them.
        for (key, field) in [(server.secret.clone(), &password), (tunnel.secret.clone(), &ssh_password)] {
            let Some(key) = key else { continue };
            let field = field.clone();
            cx.spawn(async move |_, cx| {
                if let Ok(Some(p)) = cx.background_spawn(async move { graphyne_db::secrets::get_password(&key) }).await {
                    field.update(cx, |f, cx| f.set_text(p, cx)).ok();
                }
            })
            .detach();
        }
        let mut subs = Vec::new();
        for field in [&name, &host, &port, &database, &user, &password, &ssh_host, &ssh_port, &ssh_user, &ssh_key, &ssh_password] {
            subs.push(cx.subscribe_in(field, window, |this, _, ev: &TextInputEvent, window, cx| match ev {
                TextInputEvent::Submit => this.save(window, cx),
                TextInputEvent::Cancel => cx.emit(FormEvent::Closed),
                TextInputEvent::Changed(_) => {
                    if this.status.as_ref().is_some_and(|(_, t)| *t != Tone::Info) {
                        this.status = None;
                        cx.notify();
                    }
                }
                _ => {}
            }));
        }
        window.focus(&host.focus_handle(cx));
        Self {
            focus_handle: cx.focus_handle(),
            store,
            editing,
            engine,
            name,
            host,
            port,
            database,
            user,
            password,
            ssl: server.ssl,
            ssh: server.ssh.is_some(),
            ssh_host,
            ssh_port,
            ssh_user,
            ssh_key,
            ssh_password,
            status: None,
            testing: false,
            _subs: subs,
        }
    }

    /// The fields in Tab order (the SSH ones only while the tunnel is on).
    fn fields(&self) -> Vec<&Entity<TextInput>> {
        let mut fields = vec![&self.name, &self.host, &self.port, &self.database, &self.user, &self.password];
        if self.ssh {
            fields.extend([&self.ssh_host, &self.ssh_port, &self.ssh_user, &self.ssh_key, &self.ssh_password]);
        }
        fields
    }

    fn set_ssh(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if on == self.ssh {
            return;
        }
        self.ssh = on;
        self.status = None;
        if on && self.ssh_host.read(cx).text().trim().is_empty() {
            window.focus(&self.ssh_host.focus_handle(cx));
        }
        cx.notify();
    }

    /// Pick the private key file (starting in ~/.ssh when there is one).
    fn browse_key(&mut self, cx: &mut Context<Self>) {
        let ssh_dir = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| std::path::PathBuf::from(h).join(".ssh")).filter(|d| d.is_dir());
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use Key".into()), directory: ssh_dir });
        let field = self.ssh_key.clone();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            field.update(cx, |f, cx| f.set_text(path.to_string_lossy().to_string(), cx)).ok();
        })
        .detach();
    }

    fn set_engine(&mut self, engine: Engine, cx: &mut Context<Self>) {
        if engine == self.engine {
            return;
        }
        // Defaults follow the engine unless the user typed their own.
        let old = self.engine;
        if self.port.read(cx).text().trim() == old.port().to_string() {
            self.port.update(cx, |f, cx| f.set_text(engine.port().to_string(), cx));
        }
        if self.user.read(cx).text().trim() == old.user() {
            self.user.update(cx, |f, cx| f.set_text(engine.user(), cx));
        }
        self.database.update(cx, |f, cx| f.set_placeholder(engine.database_hint(), cx));
        self.engine = engine;
        self.status = None;
        cx.notify();
    }

    fn kind(&self, cx: &App) -> Result<DbKind, String> {
        let text = |f: &Entity<TextInput>| f.read(cx).text().trim().to_string();
        let port = match text(&self.port) {
            p if p.is_empty() => self.engine.port(),
            p => p.parse::<u16>().ok().filter(|p| *p > 0).ok_or("The port must be a number from 1 to 65535")?,
        };
        let host = text(&self.host);
        let ssh = if self.ssh {
            let host = text(&self.ssh_host);
            if host.is_empty() {
                return Err("Enter the SSH host, or turn the SSH tunnel off".into());
            }
            let port = match text(&self.ssh_port) {
                p if p.is_empty() => 22,
                p => p.parse::<u16>().ok().filter(|p| *p > 0).ok_or("The SSH port must be a number from 1 to 65535")?,
            };
            // A path pasted from Explorer ("Copy as path") comes in quotes.
            let key = text(&self.ssh_key).trim_matches('"').to_string();
            Some(SshConfig { host, port, user: text(&self.ssh_user), key_file: (!key.is_empty()).then_some(key), secret: None })
        } else {
            None
        };
        let server = ServerConfig { host: if host.is_empty() { "localhost".into() } else { host }, port, database: text(&self.database), user: text(&self.user), ssl: self.ssl, secret: None, ssh, via: None };
        Ok(match self.engine {
            Engine::Postgres => DbKind::Postgres(server),
            Engine::Mysql => DbKind::Mysql(server),
        })
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        if self.testing {
            return;
        }
        let kind = match self.kind(cx) {
            Ok(kind) => kind,
            Err(e) => {
                self.status = Some((e.into(), Tone::Error));
                cx.notify();
                return;
            }
        };
        let password = self.password.read(cx).text().to_string();
        let ssh_password = self.ssh_password.read(cx).text().to_string();
        let through_ssh = self.ssh;
        let config = DbConfig { id: "test".into(), name: "test".into(), kind };
        self.testing = true;
        self.status = Some((if through_ssh { "Opening the SSH tunnel…" } else { "Connecting…" }.into(), Tone::Info));
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let session = DbSession::open_with(&config, (!password.is_empty()).then_some(password), (!ssh_password.is_empty()).then_some(ssh_password))?;
                    session.version().recv().map_err(|_| "the connection was closed".to_string())?
                })
                .await;
            this.update(cx, |this, cx| {
                this.testing = false;
                this.status = Some(match result {
                    Ok(version) => (format!("Connected: {version}").into(), Tone::Ok),
                    Err(e) => (e.into(), Tone::Error),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn save(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let kind = match self.kind(cx) {
            Ok(kind) => kind,
            Err(e) => {
                self.status = Some((e.into(), Tone::Error));
                cx.notify();
                return;
            }
        };
        let name = self.name.read(cx).text().to_string();
        let password = self.password.read(cx).text().to_string();
        let ssh_password = if self.ssh { self.ssh_password.read(cx).text().to_string() } else { String::new() };
        let editing = self.editing.clone();
        let id = self.store.update(cx, |s, cx| s.save_server(editing.as_deref(), &name, kind, password, ssh_password, cx));
        cx.emit(FormEvent::Saved(id));
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key != "tab" {
            return;
        }
        // Tab / Shift+Tab move between the fields.
        let fields = self.fields();
        let current = fields.iter().position(|f| f.focus_handle(cx).is_focused(window)).unwrap_or(0);
        let next = if ev.keystroke.modifiers.shift { (current + fields.len() - 1) % fields.len() } else { (current + 1) % fields.len() };
        window.focus(&fields[next].focus_handle(cx));
        cx.stop_propagation();
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = |text: &'static str| div().flex_none().w(px(96.)).text_color(color(c::TEXT_MUTED)).child(text);
        let field = |input: &Entity<TextInput>, window: &Window, cx: &App| {
            let focused = input.focus_handle(cx).is_focused(window);
            div()
                .flex()
                .flex_1()
                .min_w(px(0.))
                .items_center()
                .h(px(30.))
                .px(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(if focused { color(c::TAB_FOCUSED_BORDER) } else { color(c::POPUP_BORDER) })
                .bg(color(c::ISLAND))
                .child(input.clone())
        };
        let row = || div().flex().items_center().gap(px(10.));
        let segment = |id: SharedString, text: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(28.))
                .px(px(12.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(theme::UI_SMALL))
                .when(on, |d| d.bg(color(c::ACCENT)).text_color(color(0xFFFFFF)))
                .when(!on, |d| d.text_color(color(c::TEXT)).hover(|s| s.bg(color(c::HOVER))))
                .child(text)
        };
        let button = |id: &'static str, text: &'static str, primary: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(30.))
                .px(px(14.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(theme::UI_SMALL))
                .when(primary, |d| d.bg(color(c::ACCENT)).text_color(color(0xFFFFFF)).hover(|s| s.bg(color_a(c::ACCENT, 0.85))))
                .when(!primary, |d| d.border_1().border_color(color(c::POPUP_BORDER)).text_color(color(c::TEXT)).hover(|s| s.bg(color(c::HOVER))))
                .child(text)
        };
        let engine = self.engine;
        let ssl = self.ssl;
        let ssh = self.ssh;
        let ssl_tip = |mode: SslMode| match mode {
            SslMode::Prefer => "Encrypt when the server can (the certificate is not checked)",
            SslMode::Require => "Always encrypt (the certificate is not checked)",
            SslMode::VerifyFull => "Always encrypt and check the certificate and host name",
            SslMode::Disable => "Never encrypt",
        };
        let status = self.status.clone().map(|(text, tone)| {
            let tint = match tone {
                Tone::Info => color(c::TEXT_MUTED),
                Tone::Ok => color(c::GREEN),
                Tone::Error => color(c::RED),
            };
            div().text_size(px(theme::UI_SMALL)).text_color(tint).child(text)
        });
        let title = if self.editing.is_some() { "Edit Connection" } else { "New Connection" };
        let card = div()
            .id("connection-form")
            .occlude()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .w(px(500.))
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(12.))
            .bg(color(c::POPUP))
            .border_1()
            .border_color(color(c::POPUP_BORDER))
            .shadow_lg()
            .font_family(theme::fonts().ui)
            .text_size(px(theme::UI_TEXT))
            .text_color(color(c::TEXT))
            .child(div().text_size(px(15.)).font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(
                row().child(label("Database")).child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .p(px(3.))
                        .rounded(px(8.))
                        .bg(color(c::ISLAND))
                        .child(segment("engine-pg".into(), "PostgreSQL", engine == Engine::Postgres).on_click(cx.listener(|this, _, _, cx| this.set_engine(Engine::Postgres, cx))))
                        .child(segment("engine-my".into(), "MySQL / MariaDB", engine == Engine::Mysql).on_click(cx.listener(|this, _, _, cx| this.set_engine(Engine::Mysql, cx)))),
                ),
            )
            .child(row().child(label("Name")).child(field(&self.name, window, cx)))
            .child(row().child(label("Host")).child(field(&self.host, window, cx)).child(div().flex_none().text_color(color(c::TEXT_MUTED)).child("Port")).child(div().flex_none().w(px(80.)).child(field(&self.port, window, cx))))
            .child(row().child(label("Database")).child(field(&self.database, window, cx)))
            .child(row().child(label("User")).child(field(&self.user, window, cx)))
            .child(row().child(label("Password")).child(field(&self.password, window, cx)))
            .child(
                row().child(label("SSL")).child(div().flex().gap(px(4.)).p(px(3.)).rounded(px(8.)).bg(color(c::ISLAND)).children(SslMode::ALL.map(|mode| {
                    segment(SharedString::from(format!("ssl-{}", mode.label())), mode.label(), ssl == mode).tooltip(tooltip(ssl_tip(mode), None)).on_click(cx.listener(move |this, _, _, cx| {
                        this.ssl = mode;
                        cx.notify();
                    }))
                }))),
            )
            .child(
                row().child(label("SSH tunnel")).child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .p(px(3.))
                        .rounded(px(8.))
                        .bg(color(c::ISLAND))
                        .child(segment("ssh-off".into(), "Off", !ssh).on_click(cx.listener(|this, _, window, cx| this.set_ssh(false, window, cx))))
                        .child(
                            segment("ssh-on".into(), "On", ssh)
                                .tooltip(tooltip("Reach the database through an SSH host (the system's OpenSSH client)", None))
                                .on_click(cx.listener(|this, _, window, cx| this.set_ssh(true, window, cx))),
                        ),
                ),
            )
            .when(ssh, |card| {
                card.child(
                    row()
                        .child(label("SSH host"))
                        .child(field(&self.ssh_host, window, cx))
                        .child(div().flex_none().text_color(color(c::TEXT_MUTED)).child("Port"))
                        .child(div().flex_none().w(px(80.)).child(field(&self.ssh_port, window, cx))),
                )
                .child(row().child(label("SSH user")).child(field(&self.ssh_user, window, cx)))
                .child(
                    row().child(label("Key file")).child(field(&self.ssh_key, window, cx)).child(
                        button("form-ssh-key", "…", false).tooltip(tooltip("Choose the private key file", None)).on_click(cx.listener(|this, _, _, cx| this.browse_key(cx))),
                    ),
                )
                .child(row().child(label("SSH password")).child(field(&self.ssh_password, window, cx)))
                .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child("Host and Port above are as the SSH host sees them (often localhost)."))
            })
            .child(div().text_size(px(11.5)).text_color(color(c::TEXT_DIM)).child("Passwords are kept in the Windows Credential Manager, not in Graphyne's files."))
            .children(status)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(4.))
                    .child(button("form-test", if self.testing { "Testing…" } else { "Test Connection" }, false).on_click(cx.listener(|this, _, _, cx| this.test(cx))))
                    .child(div().flex_1())
                    .child(button("form-cancel", "Cancel", false).on_click(cx.listener(|_, _, _, cx| cx.emit(FormEvent::Closed))))
                    .child(button("form-save", "Save", true).on_click(cx.listener(|this, _, window, cx| this.save(window, cx)))),
            );
        // Dim everything behind; a click there closes the dialog.
        div()
            .id("connection-form-backdrop")
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(color_a(0x000000, 0.45))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(FormEvent::Closed)))
            .child(card)
    }
}
