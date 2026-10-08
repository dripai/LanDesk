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
    trust: Option<(String, oneshot::Sender<bool>)>,
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
            Ok((Settings::load(&path)?, path))
        });
        let (settings, settings_path, startup_error) = match loaded {
            Ok((settings, path)) => (settings, Some(path), None),
            Err(error) => (Settings::default(), None, Some(format!("{error:#}"))),
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
            password: input("", "仅用于本次 SSH 连接", true, window, cx),
            status: startup_error.clone().unwrap_or_else(|| "未连接".into()),
            settings,
            settings_path,
            startup_error,
            runtime,
            events,
            sender,
            cancel: None,
            trust: None,
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
    fn save(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let value = self.read_settings(cx)?;
        value.save(
            self.settings_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("请先修复启动错误"))?,
        )?;
        self.settings = value;
        Ok(())
    }
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.startup_error.is_some() {
            return;
        }
        if let Err(error) = self.save(cx) {
            self.status = format!("{error:#}");
            cx.notify();
            return;
        }
        let password = Zeroizing::new(self.password.read(cx).value().to_string());
        if password.is_empty() {
            self.status = "请输入 Mac SSH 密码".into();
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
        self.trust = None;
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
                Event::Trust { fingerprint, reply } => {
                    self.trust = Some((fingerprint, reply));
                    self.status = "首次连接，请核对 Mac SSH 主机指纹".into();
                    if let Err(error) = tray::show(window) {
                        self.status = error.to_string();
                    }
                }
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
                    self.trust = None;
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
        let fields = [
            ("Mac 地址", &self.host),
            ("用户名", &self.user),
            ("SSH 端口", &self.port),
            ("SSH 密码", &self.password),
        ];
        let mut content = div()
            .v_flex()
            .w_full()
            .p_6()
            .gap_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("LanDeskClient"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("连接 Mac，打开远程桌面"),
            );
        for (label, state) in fields {
            content = content.child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(div().text_sm().child(label))
                    .child(Input::new(state).disabled(busy || self.startup_error.is_some())),
            );
        }
        content = content
            .child(
                Checkbox::new("auto-browser")
                    .label("连接后打开浏览器")
                    .checked(self.settings.open_browser)
                    .disabled(busy)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.settings.open_browser = *checked;
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("tray")
                    .label("最小化或关闭窗口时留在托盘")
                    .checked(self.settings.minimize_to_tray)
                    .disabled(busy)
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
                            .disabled(busy || self.startup_error.is_some())
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
                            .disabled(busy || self.startup_error.is_some())
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
        if let Some((fingerprint, _)) = &self.trust {
            content = content.child(
                div()
                    .v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .child(div().text_sm().child(format!(
                        "{}:{}\n{fingerprint}",
                        self.settings.host, self.settings.ssh_port
                    )))
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("trust")
                                    .primary()
                                    .label("指纹一致，信任并连接")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some((_, reply)) = this.trust.take() {
                                            let _ = reply.send(true);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("reject")
                                    .label("取消")
                                    .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                            ),
                    ),
            );
        }
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
                    size(px(560.), px(760.)),
                    cx,
                ))),
                window_min_size: Some(size(px(520.), px(720.))),
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
