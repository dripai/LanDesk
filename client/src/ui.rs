use crate::tray::{self, Action, Tray};
use gpui_kit::component::{
    button::*,
    checkbox::Checkbox,
    input::{Input, InputState},
    scroll::ScrollableElement,
    *,
};
use gpui_kit::*;
use landesk_client::{
    credentials::{self, SavedPassword},
    gateway,
    settings::{Connections, Settings},
    tunnel::{self, Event},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::{runtime::Runtime, sync::oneshot};
use zeroize::Zeroizing;

struct Session {
    cancel: Option<oneshot::Sender<()>>,
    busy: bool,
    ready: bool,
    status: String,
}
enum Update {
    Session(String, Event),
    Gateway(String),
}
struct Client {
    name: Entity<InputState>,
    host: Entity<InputState>,
    user: Entity<InputState>,
    port: Entity<InputState>,
    password: Entity<InputState>,
    config: Connections,
    selected: Option<String>,
    editing: bool,
    delete_pending: bool,
    open_browser: bool,
    remember_password: bool,
    settings_path: Option<PathBuf>,
    startup_error: Option<String>,
    runtime: Arc<Runtime>,
    events: mpsc::Receiver<Update>,
    sender: mpsc::Sender<Update>,
    sessions: HashMap<String, Session>,
    routes: gateway::Routes,
    gateway_stop: Option<oneshot::Sender<()>>,
    quitting: bool,
    status: String,
    tray: Option<Tray>,
}
fn input(
    value: &str,
    placeholder: &str,
    secret: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| {
        let mut state = InputState::new(window, cx)
            .placeholder(placeholder.to_owned())
            .masked(secret);
        state.set_value(value.to_owned(), window, cx);
        state
    })
}
impl Client {
    fn new(runtime: Arc<Runtime>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let loaded = Settings::directory().and_then(|dir| {
            let path = dir.join("settings.json");
            let (config, migrate) = Connections::load(&path)?;
            if migrate {
                credentials::migrate(&config, &path)?;
            }
            Ok((config, path))
        });
        let (config, settings_path, mut startup_error) = match loaded {
            Ok((config, path)) => (config, Some(path), None),
            Err(error) => (Connections::default(), None, Some(format!("{error:#}"))),
        };
        let (tray, tray_error) = match Tray::new() {
            Ok(tray) => (Some(tray), None),
            Err(error) => (None, Some(format!("托盘初始化失败：{error:#}"))),
        };
        startup_error = startup_error.or(tray_error);
        let (sender, events) = mpsc::channel();
        let routes = gateway::Routes::default();
        let mut gateway_stop = None;
        if startup_error.is_none() {
            match runtime.block_on(tokio::net::TcpListener::bind(("127.0.0.1", gateway::PORT))) {
                Ok(listener) => {
                    let (stop, cancelled) = oneshot::channel();
                    gateway_stop = Some(stop);
                    let routes = routes.clone();
                    let events = sender.clone();
                    runtime.spawn(async move {
                        if let Err(error) = gateway::run(listener, routes, cancelled).await {
                            let _ =
                                events.send(Update::Gateway(format!("网页入口已停止：{error:#}")));
                        }
                    });
                }
                Err(error) => {
                    startup_error = Some(format!(
                        "本机 17890 端口不可用，请关闭旧客户端或连接脚本：{error}"
                    ))
                }
            }
        }
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| {
                if this.quitting {
                    return true;
                }
                if this.config.minimize_to_tray && this.tray.is_some() {
                    if let Err(error) = tray::hide(window) {
                        this.status = error.to_string();
                        cx.notify();
                    }
                } else {
                    this.quit(cx);
                }
                false
            })
            .unwrap_or(true)
        });
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if cx
                    .update(|window, cx| this.update(cx, |this, cx| this.poll(window, cx)))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let initial = config.profiles.first().cloned().unwrap_or_default();
        let selected = config.profiles.first().map(Settings::id);
        let remembered = selected
            .as_ref()
            .map(|id| credentials::read(id))
            .transpose();
        let remember_password = match remembered {
            Ok(saved) => saved.flatten().is_some_and(|saved| saved.matches(&initial)),
            Err(error) => {
                startup_error = Some(format!("{error:#}"));
                false
            }
        };
        Self {
            name: input(&initial.name, "例如：开发 Mac", false, window, cx),
            host: input(&initial.host, "IP 或主机名", false, window, cx),
            user: input(&initial.user, "Mac 用户名", false, window, cx),
            port: input(&initial.ssh_port.to_string(), "22", false, window, cx),
            password: input("", "已记住的密码可留空", true, window, cx),
            editing: selected.is_none(),
            selected,
            delete_pending: false,
            open_browser: initial.open_browser,
            remember_password,
            status: startup_error.clone().unwrap_or_else(|| "".into()),
            config,
            settings_path,
            startup_error,
            runtime,
            events,
            sender,
            sessions: HashMap::new(),
            routes,
            gateway_stop,
            quitting: false,
            tray,
        }
    }
    fn current_busy(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .is_some_and(|s| s.busy)
    }
    fn select(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let profile = id
            .as_ref()
            .and_then(|id| self.config.profiles.iter().find(|p| p.id() == *id))
            .cloned()
            .unwrap_or_default();
        let remembered = id.as_ref().map(|id| credentials::read(id)).transpose();
        let remember = match remembered {
            Ok(saved) => saved.flatten().is_some_and(|saved| saved.matches(&profile)),
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
                return;
            }
        };
        for (state, value) in [
            (&self.name, profile.name),
            (&self.host, profile.host),
            (&self.user, profile.user),
            (&self.port, profile.ssh_port.to_string()),
            (&self.password, String::new()),
        ] {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.open_browser = profile.open_browser;
        self.remember_password = remember;
        self.editing = id.is_none();
        self.selected = id;
        self.delete_pending = false;
        self.status.clear();
        cx.notify();
    }
    fn read_settings(&self, cx: &App) -> anyhow::Result<Settings> {
        let value = Settings {
            name: self.name.read(cx).value().trim().to_string(),
            host: self.host.read(cx).value().trim().to_string(),
            user: self.user.read(cx).value().trim().to_string(),
            ssh_port: self
                .port
                .read(cx)
                .value()
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("SSH 端口必须为 1–65535"))?,
            open_browser: self.open_browser,
            minimize_to_tray: self.config.minimize_to_tray,
        };
        value.validate()?;
        Ok(value)
    }
    fn password_for(&self, settings: &Settings, cx: &App) -> anyhow::Result<Zeroizing<String>> {
        let typed = self.password.read(cx).value();
        if !typed.is_empty() {
            return Ok(Zeroizing::new(typed.to_string()));
        }
        if self.remember_password
            && let Some(saved) = credentials::read(&settings.id())?
            && saved.matches(settings)
        {
            return Ok(saved.password());
        }
        anyhow::bail!("请输入当前 Mac 用户的 SSH 密码")
    }
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let value = self.read_settings(cx)?;
        let saved = if self.remember_password {
            Some(SavedPassword::new(&value, &self.password_for(&value, cx)?))
        } else {
            None
        };
        let mut config = self.config.clone();
        let id = config.replace(self.selected.as_deref(), value)?;
        let mut changes = vec![(id.clone(), saved)];
        if let Some(old) = &self.selected
            && old != &id
        {
            changes.push((old.clone(), None));
        }
        credentials::commit(
            &config,
            self.settings_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("请先修复启动错误"))?,
            changes,
        )?;
        if let Some(old) = &self.selected
            && old != &id
        {
            self.sessions.remove(old);
        }
        self.config = config;
        self.selected = Some(id);
        self.editing = false;
        if self.remember_password {
            self.password
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        Ok(())
    }
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_busy() || self.startup_error.is_some() || self.editing {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(settings) = self.config.profiles.iter().find(|p| p.id() == id).cloned() else {
            return;
        };
        let password = match self.password_for(&settings, cx) {
            Ok(password) => password,
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
                return;
            }
        };
        // Apply the remember checkbox independently to this profile when connecting.
        let saved = self
            .remember_password
            .then(|| SavedPassword::new(&settings, &password));
        if let Err(error) = credentials::commit(
            &self.config,
            self.settings_path.as_deref().unwrap(),
            vec![(id.clone(), saved)],
        ) {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        self.password
            .update(cx, |state, cx| state.set_value("", window, cx));
        let (cancel, cancelled) = oneshot::channel();
        self.sessions.insert(
            id.clone(),
            Session {
                cancel: Some(cancel),
                busy: true,
                ready: false,
                status: "正在连接…".into(),
            },
        );
        self.status.clear();
        let events = self.sender.clone();
        let routes = self.routes.clone();
        let known_hosts = self
            .settings_path
            .as_ref()
            .unwrap()
            .with_file_name("known_hosts");
        // Bridge per-session events without allowing one session to overwrite another.
        self.runtime.spawn(async move {
            let (tx, rx) = mpsc::channel();
            let task = tunnel::run(settings, password, known_hosts, tx, cancelled, routes);
            tokio::pin!(task);
            let mut tick = tokio::time::interval(Duration::from_millis(30));
            loop {
                tokio::select! {
                    result = &mut task => {
                        while let Ok(event) = rx.try_recv() { let _ = events.send(Update::Session(id.clone(), event)); }
                        let _ = events.send(Update::Session(id, Event::Stopped(result))); break;
                    }
                    _ = tick.tick() => { while let Ok(event) = rx.try_recv() { let _ = events.send(Update::Session(id.clone(), event)); } }
                }
            }
        });
        cx.notify();
    }
    fn disconnect_id(&mut self, id: &str) {
        if let Some(session) = self.sessions.get_mut(id) {
            if let Some(cancel) = session.cancel.take() {
                let _ = cancel.send(());
                session.status = "正在断开…".into();
            }
            session.ready = false;
        }
    }
    fn disconnect_all(&mut self) {
        for id in self.sessions.keys().cloned().collect::<Vec<_>>() {
            self.disconnect_id(&id);
        }
    }
    fn quit(&mut self, cx: &mut Context<Self>) {
        self.quitting = true;
        self.disconnect_all();
        if !self.sessions.values().any(|s| s.busy) {
            if let Some(stop) = self.gateway_stop.take() {
                let _ = stop.send(());
            }
            cx.quit();
        }
    }
    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_busy() {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        let mut config = self.config.clone();
        config.profiles.retain(|p| p.id() != id);
        match credentials::commit(
            &config,
            self.settings_path.as_deref().unwrap(),
            vec![(id.clone(), None)],
        ) {
            Ok(()) => {
                self.config = config;
                self.sessions.remove(&id);
                self.select(self.config.profiles.first().map(Settings::id), window, cx);
            }
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
            }
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Ok(update) = self.events.try_recv() {
            match update {
                Update::Gateway(error) => {
                    self.startup_error = Some(error.clone());
                    self.status = error;
                    self.disconnect_all();
                }
                Update::Session(id, event) => {
                    if let Some(session) = self.sessions.get_mut(&id) {
                        match event {
                            Event::Status(status) => session.status = status,
                            Event::Ready => {
                                if session.cancel.is_none() {
                                    continue;
                                }
                                session.ready = true;
                                session.status = "已连接 · 无远控连接 30 秒后断开".into();
                                if let Some(profile) =
                                    self.config.profiles.iter().find(|p| p.id() == id)
                                    && profile.open_browser
                                {
                                    cx.open_url(&profile.viewer_url());
                                }
                            }
                            Event::Stopped(result) => {
                                session.cancel = None;
                                session.busy = false;
                                session.ready = false;
                                session.status = match result {
                                    Ok(tunnel::StopReason::Disconnected) => "已断开".into(),
                                    Ok(tunnel::StopReason::IdleTimeout) => {
                                        "空闲 30 秒，SSH 已关闭，请重新连接".into()
                                    }
                                    Err(error) => format!("{error:#}"),
                                };
                            }
                        }
                    }
                }
            }
            cx.notify();
        }
        if self.quitting && !self.sessions.values().any(|s| s.busy) {
            self.quit(cx);
            return;
        }
        while let Some(action) = self.tray.as_ref().and_then(Tray::next) {
            match action {
                Action::Show => {
                    if let Err(error) = tray::show(window) {
                        self.status = error.to_string();
                        cx.notify();
                    }
                }
                Action::Disconnect => {
                    self.disconnect_all();
                    cx.notify();
                }
                Action::Quit => self.quit(cx),
            }
        }
        if self.config.minimize_to_tray
            && self.tray.is_some()
            && let Err(error) = tray::minimized(window).and_then(|minimized| {
                if minimized {
                    tray::hide(window)
                } else {
                    Ok(())
                }
            })
        {
            self.config.minimize_to_tray = false;
            self.status = error.to_string();
            cx.notify();
        }
    }
}
impl Render for Client {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.current_busy();
        let blocked = self.startup_error.is_some() || self.quitting;
        let ready = self
            .selected
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .is_some_and(|s| s.ready);
        let mut sidebar = div()
            .v_flex()
            .w(px(220.))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .p_3()
            .gap_2()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().child("服务器"))
                    .child(
                        Button::new("add")
                            .label("+")
                            .disabled(self.editing || blocked)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.select(None, window, cx)),
                            ),
                    ),
            );
        for profile in &self.config.profiles {
            let id = profile.id();
            let selected = self.selected.as_ref() == Some(&id);
            let state = self
                .sessions
                .get(&id)
                .map(|s| {
                    if s.ready {
                        "已连接"
                    } else if s.busy {
                        "连接中"
                    } else {
                        "未连接"
                    }
                })
                .unwrap_or("未连接");
            let name = format!("{} · {state}", profile.label());
            sidebar = sidebar.child(
                Button::new(SharedString::from(id.clone()))
                    .label(name)
                    .w_full()
                    .selected(selected)
                    .disabled(self.editing || blocked)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(Some(id.clone()), window, cx)
                    })),
            );
        }
        let mut detail = div().v_flex().w_full().p_6().gap_4();
        for (label, state) in [
            ("名称", &self.name),
            ("Mac 地址", &self.host),
            ("用户名", &self.user),
            ("SSH 端口", &self.port),
            ("SSH 密码", &self.password),
        ] {
            let disabled = blocked || busy || (!self.editing && label != "SSH 密码");
            detail = detail.child(
                div()
                    .h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().w(px(80.)).flex_shrink_0().text_sm().child(label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(state).disabled(disabled)),
                    ),
            );
        }
        detail = detail.child(
            div().pl(px(92.)).child(
                Checkbox::new("remember-password")
                    .label("记住密码")
                    .checked(self.remember_password)
                    .disabled(blocked || busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.remember_password = *checked;
                        cx.notify();
                    })),
            ),
        );
        detail = detail.child(
            Checkbox::new("auto-browser")
                .label("连接后打开浏览器")
                .checked(self.open_browser)
                .disabled(blocked || !self.editing)
                .on_click(cx.listener(|this, checked, _, cx| {
                    this.open_browser = *checked;
                    cx.notify();
                })),
        );
        let mut buttons = div().h_flex().gap_2();
        if self.editing {
            buttons = buttons
                .child(
                    Button::new("save")
                        .primary()
                        .label("保存")
                        .disabled(blocked)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.status = match this.save(window, cx) {
                                Ok(()) => "已保存".into(),
                                Err(error) => format!("{error:#}"),
                            };
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("cancel-edit")
                        .label("取消")
                        .disabled(blocked || self.config.profiles.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| {
                            let id = this
                                .selected
                                .clone()
                                .or_else(|| this.config.profiles.first().map(Settings::id));
                            this.select(id, window, cx);
                        })),
                );
        } else {
            buttons = buttons
                .child(
                    Button::new("connect")
                        .primary()
                        .label("连接")
                        .disabled(blocked || busy)
                        .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                )
                .child(
                    Button::new("disconnect")
                        .label("断开")
                        .disabled(!busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(id) = this.selected.clone() {
                                this.disconnect_id(&id);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("open")
                        .label("打开桌面")
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(id) = &this.selected
                                && let Some(profile) =
                                    this.config.profiles.iter().find(|p| p.id() == *id)
                            {
                                cx.open_url(&profile.viewer_url());
                            }
                        })),
                )
                .child(
                    Button::new("edit")
                        .label("编辑")
                        .disabled(blocked || busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.editing = true;
                            this.delete_pending = false;
                            this.status.clear();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("delete")
                        .label("删除")
                        .disabled(blocked || busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.delete_pending = true;
                            cx.notify();
                        })),
                );
        }
        detail = detail.child(buttons);
        if self.delete_pending {
            detail = detail.child(
                div()
                    .v_flex()
                    .gap_2()
                    .child(div().text_sm().child("删除这条连接及其已记住的密码？"))
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(Button::new("confirm-delete").label("确认删除").on_click(
                                cx.listener(|this, _, window, cx| this.delete(window, cx)),
                            ))
                            .child(Button::new("cancel-delete").label("取消").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.delete_pending = false;
                                    cx.notify();
                                }),
                            )),
                    ),
            );
        }
        let status = if !self.status.is_empty() {
            self.status.clone()
        } else {
            self.selected
                .as_ref()
                .and_then(|id| self.sessions.get(id))
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "未连接".into())
        };
        detail = detail.child(div().text_sm().child(status)).child(
            div().mt_4().child(
                Checkbox::new("tray")
                    .label("最小化或关闭窗口时留在托盘")
                    .checked(self.config.minimize_to_tray)
                    .disabled(blocked)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        let mut next = this.config.clone();
                        next.minimize_to_tray = *checked;
                        match next.save(this.settings_path.as_deref().unwrap()) {
                            Ok(()) => this.config = next,
                            Err(error) => this.status = format!("{error:#}"),
                        }
                        cx.notify();
                    })),
            ),
        );
        div()
            .h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .id("server-list-scroll")
                    .h_full()
                    .flex_shrink_0()
                    .overflow_y_scrollbar()
                    .child(sidebar),
            )
            .child(
                div()
                    .id("connection-detail-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scrollbar()
                    .child(detail),
            )
    }
}

pub fn run() {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => Arc::new(runtime),
        Err(error) => {
            fatal(&error.to_string());
            return;
        }
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("LanDeskClient".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(880.), px(540.)),
                    cx,
                ))),
                window_min_size: Some(size(px(820.), px(520.))),
                ..Default::default()
            };
            if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Client::new(runtime.clone(), window, cx))
            }) {
                fatal(&error.to_string());
                cx.quit();
            }
            cx.activate(true);
        });
}

fn fatal(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let title: Vec<u16> = "LanDeskClient\0".encode_utf16().collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR | MB_OK,
        );
    }
}
