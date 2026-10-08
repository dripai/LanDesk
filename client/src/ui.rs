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
    settings::Settings,
    tunnel::{self, Event},
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::{runtime::Runtime, sync::oneshot};
use zeroize::Zeroizing;

struct Client {
    host: Entity<InputState>,
    user: Entity<InputState>,
    port: Entity<InputState>,
    password: Entity<InputState>,
    settings: Settings,
    settings_path: Option<PathBuf>,
    startup_error: Option<String>,
    runtime: Arc<Runtime>,
    events: mpsc::Receiver<Event>,
    sender: mpsc::Sender<Event>,
    cancel: Option<oneshot::Sender<()>>,
    remember_password: bool,
    busy: bool,
    ready: bool,
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
            let settings = Settings::load(&path)?;
            let remembered = credentials::read()?.is_some_and(|saved| saved.matches(&settings));
            Ok((settings, path, remembered))
        });
        let (settings, settings_path, remember_password, startup_error) = match loaded {
            Ok((settings, path, remembered)) => (settings, Some(path), remembered, None),
            Err(error) => (Settings::default(), None, false, Some(format!("{error:#}"))),
        };
        let (tray, tray_error) = match Tray::new() {
            Ok(tray) => (Some(tray), None),
            Err(error) => (None, Some(format!("托盘初始化失败：{error:#}"))),
        };
        let startup_error = startup_error.or(tray_error);
        let (sender, events) = mpsc::channel();
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| {
                if this.quitting {
                    return true;
                }
                if this.settings.minimize_to_tray && this.tray.is_some() {
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
        Self {
            host: input(
                &settings.host,
                "例如 192.0.2.10 或 Mac.local",
                false,
                window,
                cx,
            ),
            user: input(&settings.user, "Mac 用户名", false, window, cx),
            port: input(&settings.ssh_port.to_string(), "22", false, window, cx),
            password: input("", "输入密码；已记住的密码可留空", true, window, cx),
            status: startup_error.clone().unwrap_or_else(|| "未连接".into()),
            settings,
            settings_path,
            startup_error,
            runtime,
            events,
            sender,
            cancel: None,
            remember_password,
            busy: false,
            ready: false,
            quitting: false,
            tray,
        }
    }
    fn read_settings(&self, cx: &App) -> anyhow::Result<Settings> {
        let value = Settings {
            host: self.host.read(cx).value().trim().to_string(),
            user: self.user.read(cx).value().trim().to_string(),
            ssh_port: self
                .port
                .read(cx)
                .value()
                .parse()
                .map_err(|_| anyhow::anyhow!("SSH 端口必须为 1–65535"))?,
            ..self.settings.clone()
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
            && let Some(saved) = credentials::read()?
            && saved.matches(settings)
        {
            return Ok(saved.password());
        }
        anyhow::bail!("请输入当前 Mac 用户的 SSH 密码")
    }
    fn save(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let value = self.read_settings(cx)?;
        let saved = if self.remember_password {
            Some(SavedPassword::new(&value, &self.password_for(&value, cx)?))
        } else {
            None
        };
        credentials::save(
            &value,
            self.settings_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("请先修复启动错误"))?,
            saved.as_ref(),
        )?;
        self.settings = value;
        Ok(())
    }
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.startup_error.is_some() {
            return;
        }
        let password = match self
            .read_settings(cx)
            .and_then(|settings| self.password_for(&settings, cx))
        {
            Ok(password) => password,
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
                return;
            }
        };
        if let Err(error) = self.save(cx) {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        self.password
            .update(cx, |state, cx| state.set_value("", window, cx));
        let (cancel, cancelled) = oneshot::channel();
        self.cancel = Some(cancel);
        self.busy = true;
        let settings = self.settings.clone();
        let events = self.sender.clone();
        let known_hosts = self
            .settings_path
            .as_ref()
            .unwrap()
            .with_file_name("known_hosts");
        self.runtime.spawn(async move {
            let result =
                tunnel::run(settings, password, known_hosts, events.clone(), cancelled).await;
            let _ = events.send(Event::Stopped(result));
        });
        self.status = "正在连接…".into();
        cx.notify();
    }
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
            self.status = "正在断开…".into();
        }
        self.ready = false;
        cx.notify();
    }
    fn quit(&mut self, cx: &mut Context<Self>) {
        self.quitting = true;
        if self.busy {
            self.disconnect(cx);
        } else {
            cx.quit();
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Status(status) => self.status = status,
                Event::Ready => {
                    self.ready = true;
                    self.status = "已连接 · SSH 加密隧道运行中".into();
                    if self.settings.open_browser {
                        cx.open_url(tunnel::VIEWER_URL);
                    }
                }
                Event::Stopped(result) => {
                    self.cancel = None;
                    self.busy = false;
                    self.ready = false;
                    self.status = result
                        .err()
                        .map(|e| format!("{e:#}"))
                        .unwrap_or_else(|| "已断开".into());
                    if self.quitting {
                        cx.quit();
                    }
                }
            }
            cx.notify();
        }
        while let Some(action) = self.tray.as_ref().and_then(Tray::next) {
            match action {
                Action::Show => {
                    if let Err(error) = tray::show(window) {
                        self.status = error.to_string();
                        cx.notify();
                    }
                }
                Action::Disconnect => self.disconnect(cx),
                Action::Quit => self.quit(cx),
            }
        }
        if self.settings.minimize_to_tray && self.tray.is_some() {
            let result = tray::minimized(window).and_then(|minimized| {
                if minimized {
                    tray::hide(window)
                } else {
                    Ok(())
                }
            });
            if let Err(error) = result {
                self.settings.minimize_to_tray = false;
                self.status = error.to_string();
                cx.notify();
            }
        }
    }
}

impl Render for Client {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let blocked = busy || self.startup_error.is_some();
        let mut content = div()
            .v_flex()
            .w_full()
            .p_6()
            .gap_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        for (label, state) in [
            ("Mac 地址", &self.host),
            ("用户名", &self.user),
            ("SSH 端口", &self.port),
            ("SSH 密码", &self.password),
        ] {
            content = content.child(
                div()
                    .h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().w(px(80.)).flex_shrink_0().text_sm().child(label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(state).disabled(blocked)),
                    ),
            );
        }
        content = content
            .child(
                div().pl(px(92.)).child(
                    Checkbox::new("remember-password")
                        .label("记住密码")
                        .checked(self.remember_password)
                        .disabled(blocked)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            this.remember_password = *checked;
                            cx.notify();
                        })),
                ),
            )
            .child(
                Checkbox::new("auto-browser")
                    .label("连接后打开浏览器")
                    .checked(self.settings.open_browser)
                    .disabled(blocked)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.settings.open_browser = *checked;
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("tray")
                    .label("最小化或关闭窗口时留在托盘")
                    .checked(self.settings.minimize_to_tray)
                    .disabled(blocked)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.settings.minimize_to_tray = *checked;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        Button::new("connect")
                            .primary()
                            .label("连接")
                            .disabled(blocked)
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    )
                    .child(
                        Button::new("disconnect")
                            .label("断开")
                            .disabled(!busy)
                            .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                    )
                    .child(
                        Button::new("open")
                            .label("打开远程桌面")
                            .disabled(!self.ready)
                            .on_click(|_, _, cx| cx.open_url(tunnel::VIEWER_URL)),
                    )
                    .child(
                        Button::new("save")
                            .label("保存设置")
                            .disabled(blocked)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.status = match this.save(cx) {
                                    Ok(()) => "设置已保存".into(),
                                    Err(e) => format!("{e:#}"),
                                };
                                cx.notify();
                            })),
                    ),
            )
            .child(div().text_sm().child(self.status.clone()));
        div()
            .id("client-settings-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(content.flex_shrink_0())
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
                    size(px(560.), px(460.)),
                    cx,
                ))),
                window_min_size: Some(size(px(520.), px(440.))),
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
