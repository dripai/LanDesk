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
    connections::{ConnectionManager, Notification},
    credentials::{self, SavedPassword},
    settings::{Authentication, ConnectionMode, Connections, Settings},
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::runtime::Runtime;
use zeroize::Zeroizing;

struct Client {
    name: Entity<InputState>,
    host: Entity<InputState>,
    user: Entity<InputState>,
    port: Entity<InputState>,
    password: Entity<InputState>,
    key_path: Entity<InputState>,
    mode: ConnectionMode,
    use_key: bool,
    config: Connections,
    selected: Option<String>,
    editing: bool,
    delete_pending: bool,
    remember_password: bool,
    settings_path: Option<PathBuf>,
    startup_error: Option<String>,
    connections: ConnectionManager,
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
        let mut connections = ConnectionManager::new(runtime);
        if startup_error.is_none()
            && let Err(error) = connections.start_gateway()
        {
            startup_error = Some(format!("{error:#}"));
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
            name: input(&initial.name, "例如：开发服务器", false, window, cx),
            host: input(&initial.host, "IP 或主机名", false, window, cx),
            user: input(&initial.user, "SSH 用户名", false, window, cx),
            port: input(&initial.ssh_port.to_string(), "连接端口", false, window, cx),
            password: input("", "密码或私钥口令", true, window, cx),
            key_path: input(
                match &initial.authentication {
                    Authentication::PrivateKey { path } => path.to_str().unwrap_or(""),
                    Authentication::Password => "",
                },
                "私钥文件的绝对路径",
                false,
                window,
                cx,
            ),
            mode: initial.mode,
            use_key: initial.mode == ConnectionMode::Terminal
                && matches!(initial.authentication, Authentication::PrivateKey { .. }),
            editing: selected.is_none(),
            selected,
            delete_pending: false,
            remember_password,
            status: startup_error.clone().unwrap_or_else(|| "".into()),
            config,
            settings_path,
            startup_error,
            connections,
            quitting: false,
            tray,
        }
    }
    fn current_busy(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.connections.get(id))
            .is_some_and(|s| s.is_busy())
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
        self.mode = profile.mode;
        self.use_key = profile.mode == ConnectionMode::Terminal
            && matches!(profile.authentication, Authentication::PrivateKey { .. });
        let key_path = match &profile.authentication {
            Authentication::PrivateKey { path } => path.to_string_lossy().into_owned(),
            Authentication::Password => String::new(),
        };
        for (state, value) in [
            (&self.name, profile.name),
            (&self.host, profile.host),
            (&self.user, profile.user),
            (&self.port, profile.ssh_port.to_string()),
            (&self.password, String::new()),
            (&self.key_path, key_path),
        ] {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
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
            user: if self.mode == ConnectionMode::Desktop {
                String::new()
            } else {
                self.user.read(cx).value().trim().to_string()
            },
            ssh_port: self
                .port
                .read(cx)
                .value()
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("SSH 端口必须为 1–65535"))?,
            open_browser: true,
            minimize_to_tray: self.config.minimize_to_tray,
            mode: self.mode,
            authentication: if self.use_key && self.mode == ConnectionMode::Terminal {
                Authentication::PrivateKey {
                    path: PathBuf::from(self.key_path.read(cx).value().trim()),
                }
            } else {
                Authentication::Password
            },
        };
        value.validate()?;
        Ok(value)
    }
    fn password_for(&self, settings: &Settings, cx: &App) -> anyhow::Result<Zeroizing<String>> {
        let typed = self.password.read(cx).value();
        if !typed.is_empty() {
            return Ok(Zeroizing::new(typed.to_string()));
        }
        if matches!(settings.authentication, Authentication::PrivateKey { .. }) {
            return Ok(Zeroizing::new(String::new()));
        }
        if self.remember_password
            && let Some(saved) = credentials::read(&settings.id())?
            && saved.matches(settings)
        {
            return Ok(saved.password());
        }
        anyhow::bail!("请输入连接密码")
    }
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        anyhow::ensure!(!self.current_busy(), "请先断开连接再修改配置");
        let value = self.read_settings(cx)?;
        let saved = if self.remember_password && !self.use_key {
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
            self.connections.forget(old)?;
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
        let saved = (self.remember_password && !self.use_key)
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
        let known_hosts = self
            .settings_path
            .as_ref()
            .unwrap()
            .with_file_name("known_hosts");
        match self.connections.connect(settings, password, known_hosts) {
            Ok(()) => self.status.clear(),
            Err(error) => self.status = format!("{error:#}"),
        }
        cx.notify();
    }
    fn disconnect_id(&mut self, id: &str) {
        self.connections.disconnect(id);
    }
    fn disconnect_all(&mut self) {
        self.connections.disconnect_all();
    }
    fn quit(&mut self, cx: &mut Context<Self>) {
        self.quitting = true;
        self.connections.shutdown();
        if self.connections.is_idle() {
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
                if let Err(error) = self.connections.forget(&id) {
                    self.status = format!("{error:#}");
                    cx.notify();
                    return;
                }
                self.select(self.config.profiles.first().map(Settings::id), window, cx);
            }
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
            }
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for update in self.connections.poll() {
            match update {
                Notification::GatewayFailed(error) => {
                    self.startup_error = Some(error.clone());
                    self.status = error;
                }
                Notification::Ready(id) => {
                    if let Some(profile) = self.config.profiles.iter().find(|p| p.id() == id) {
                        cx.open_url(&profile.viewer_url());
                    }
                }
                Notification::Changed => {}
            }
            cx.notify();
        }
        if self.quitting && self.connections.is_idle() {
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
                .connections
                .get(&id)
                .map(|s| {
                    if s.is_ready() {
                        "已连接"
                    } else if s.is_busy() {
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
        let mut modes = div()
            .h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(80.)).text_sm().child("连接模式"));
        for (id, label, mode) in [
            ("desktop-mode", "远程桌面", ConnectionMode::Desktop),
            ("terminal-mode", "SSH 终端", ConnectionMode::Terminal),
        ] {
            modes = modes.child(
                Button::new(id)
                    .label(label)
                    .selected(self.mode == mode)
                    .disabled(blocked || busy || !self.editing)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.mode = mode;
                        if this.selected.is_none() {
                            let port = if mode == ConnectionMode::Desktop {
                                "2222"
                            } else {
                                "22"
                            };
                            this.port
                                .update(cx, |input, cx| input.set_value(port, window, cx));
                        }
                        if mode == ConnectionMode::Desktop {
                            this.use_key = false;
                        }
                        cx.notify();
                    })),
            );
        }
        detail = detail.child(modes);
        let mut auth = div()
            .h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(80.)).text_sm().child("认证方式"));
        for (id, label, key) in [("password-auth", "密码", false), ("key-auth", "私钥", true)] {
            auth = auth.child(
                Button::new(id)
                    .label(label)
                    .selected(self.use_key == key)
                    .disabled(blocked || busy || !self.editing)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.use_key != key {
                            this.use_key = key;
                            this.remember_password = false;
                            this.password
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        cx.notify();
                    })),
            );
        }
        if self.mode == ConnectionMode::Terminal {
            detail = detail.child(auth);
        }
        let secret_label = if self.use_key {
            "私钥口令"
        } else {
            if self.mode == ConnectionMode::Desktop {
                "连接密码"
            } else {
                "SSH 密码"
            }
        };
        let mut fields = vec![
            ("名称", &self.name),
            ("服务器地址", &self.host),
            ("连接端口", &self.port),
        ];
        if self.mode == ConnectionMode::Terminal {
            fields.push(("用户名", &self.user));
        }
        if self.use_key {
            fields.push(("私钥路径", &self.key_path));
        }
        fields.push((secret_label, &self.password));
        for (label, state) in fields {
            let disabled = blocked || busy || (!self.editing && label != secret_label);
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
        if !self.use_key {
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
        } else {
            detail = detail.child(
                div()
                    .pl(px(92.))
                    .text_sm()
                    .child("未加密私钥的口令留空；口令不保存。"),
            );
        }
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
                .and_then(|id| self.connections.get(id))
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
                    .w(px(220.))
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
