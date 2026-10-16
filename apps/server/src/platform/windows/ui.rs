//! GPUI management window. Privileged work never blocks the UI event loop.
use super::{
    management::{self, Job, Preferences, Request},
    tray::{self, Action, Tray},
};
use crate::settings_view::{connection_row, input, status_bar};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::{
    component::{button::*, input::InputState, *},
    *,
};
use landesk_core::application::operation::{Operation, Refresh};
use std::{
    ptr::null,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use zeroize::Zeroizing;

enum Work {
    Start,
    Save(Option<Zeroizing<String>>),
    Startup,
    Restart,
    Uninstall,
}
struct Pending {
    job: Job,
    work: Work,
}
struct Panel {
    port: Entity<InputState>,
    password: Entity<InputState>,
    loaded: bool,
    preferences: Preferences,
    tray: Option<Tray>,
    pending: Option<Pending>,
    operation: Operation<Result<()>>,
    reading: Refresh<Result<super::password_pipe::PasswordInfo>>,
    states_read: Refresh<Result<[String; 2]>>,
    states: [String; 2],
    next_states: Instant,
    status: String,
    restart_pending: bool,
    uninstalling: bool,
}
impl Panel {
    fn new(
        preferences: Preferences,
        tray: Option<Tray>,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = weak.update(cx, |this, cx| {
                if this.busy() {
                    this.message("正在操作，请等待完成", cx);
                } else if this.preferences.minimize && this.tray.is_some() {
                    if let Err(error) = tray::hide(window) {
                        this.message(format!("{error:#}"), cx);
                    }
                } else {
                    cx.quit();
                }
            });
            false
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
        let mut panel = Self {
            port: input(String::new(), window, cx),
            password: input(String::new(), window, cx),
            loaded: false,
            preferences,
            tray,
            pending: None,
            operation: Operation::new(),
            reading: Refresh::default(),
            states_read: Refresh::default(),
            states: ["正在读取…".into(), "正在读取…".into()],
            next_states: Instant::now(),
            status: error.unwrap_or_else(|| "正在读取后台状态…".into()),
            restart_pending: false,
            uninstalling: false,
        };
        match management::service_states() {
            Ok(states) if states.iter().all(|state| state == "运行中") => {
                panel.states = states;
                panel.reading.request();
            }
            Ok(_) => panel.begin(Request::Start, Work::Start, cx),
            Err(error) => panel.message(format!("{error:#}"), cx),
        }
        panel
    }
    fn busy(&self) -> bool {
        self.operation.is_busy() || self.uninstalling
    }
    fn message(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let mut text = text.into();
        if self.restart_pending && !text.contains("下次重启后生效") && !self.uninstalling {
            text.push_str("；已保存设置下次重启后生效");
        }
        if self.status != text {
            crate::diagnostics::log(&format!("Management status: {text}"));
            self.status = text;
            cx.notify();
        }
    }
    fn begin(&mut self, request: Request, work: Work, cx: &mut Context<Self>) {
        let result = (|| {
            ensure!(!self.busy(), "操作进行中");
            let job = Job::new()?;
            job.submit(&request)?;
            self.start_job(job, work)
        })();
        match result {
            Ok(()) => self.message("正在操作…", cx),
            Err(error) => {
                if let Some(tray) = &self.tray {
                    tray.update(self.preferences, !self.busy());
                }
                self.message(format!("{error:#}"), cx);
            }
        }
    }
    fn start_job(&mut self, job: Job, work: Work) -> Result<()> {
        let (exe, args) = job.invocation();
        self.operation
            .start(move || management::invoke(exe, args))?;
        self.reading.invalidate();
        self.pending = Some(Pending { job, work });
        if let Some(tray) = &self.tray {
            tray.update(self.preferences, false);
        }
        Ok(())
    }
    fn save(&mut self, random: bool, cx: &mut Context<Self>) {
        let result = (|| -> Result<()> {
            ensure!(!self.busy() && self.loaded, "连接设置尚未就绪");
            let port = if random {
                None
            } else {
                let port: u16 = self
                    .port
                    .read(cx)
                    .value()
                    .trim()
                    .parse()
                    .context("端口必须在 1–65535 之间")?;
                ensure!(port != 0, "端口必须在 1–65535 之间");
                Some(port)
            };
            let password = if random {
                landesk_core::domain::password::generate()?
            } else {
                Zeroizing::new(self.password.read(cx).value().to_string())
            };
            let job = Job::new()?;
            job.submit(&Request::Save {
                port,
                password: job.password(&password)?,
            })?;
            self.start_job(job, Work::Save(random.then_some(password)))
        })();
        match result {
            Ok(()) => self.message("正在保存连接设置…", cx),
            Err(error) => self.message(format!("{error:#}"), cx),
        }
    }
    fn uninstall(&mut self, window: &Window, cx: &mut Context<Self>) {
        // A single confirmation. System UAC is handled by the privilege adapter.
        let confirmed = tray::hwnd(window).map(|hwnd| unsafe { MessageBoxW(hwnd,
            super::desktop::wide("卸载 LanDeskServer？当前远程连接将断开，程序、配置和日志保留。将移除 LanDesk 自建的虚拟显示设备。").as_ptr(),
            super::desktop::wide("卸载确认").as_ptr(), MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2) == IDOK });
        match confirmed {
            Ok(true) => self.begin(
                Request::Uninstall {
                    owner: std::process::id(),
                    remove_display: true,
                },
                Work::Uninstall,
                cx,
            ),
            Ok(false) => {}
            Err(error) => self.message(format!("{error:#}"), cx),
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = &self.pending {
            match pending.job.progress() {
                Ok(Some(progress)) => {
                    self.message(progress.text, cx);
                    if progress.close && self.uninstalling {
                        self.pending.take().unwrap().job.retain();
                        cx.quit();
                        return;
                    }
                    if progress.error && self.uninstalling {
                        self.uninstalling = false;
                        self.pending = None;
                        if let Some(tray) = &self.tray {
                            tray.update(self.preferences, true);
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => self.message(format!("无法读取操作进度：{error:#}"), cx),
            }
        }
        if self.uninstalling {
            let result = self
                .pending
                .as_ref()
                .expect("uninstall owns request")
                .job
                .helper_running();
            if !matches!(result, Ok(true)) {
                self.uninstalling = false;
                let text = match result {
                    Ok(false) => "卸载辅助进程已退出，结果未确认，请查看 uninstall.log".to_owned(),
                    Err(error) => format!("卸载状态无法确认：{error:#}"),
                    _ => unreachable!(),
                };
                self.message(text, cx);
                // Do not delete requests if a helper's status cannot be confirmed.
                self.pending.take().unwrap().job.retain();
                if let Some(tray) = &self.tray {
                    tray.update(self.preferences, true);
                }
            }
        }
        if let Some(result) = self.operation.poll() {
            let pending = self.pending.take().expect("operation owns its request");
            if matches!(pending.work, Work::Start | Work::Restart) {
                match pending.job.settings() {
                    Ok((port, password)) => {
                        self.port.update(cx, |input, cx| {
                            input.set_value(port.to_string(), window, cx)
                        });
                        self.password.update(cx, |input, cx| {
                            input.set_value(password.to_string(), window, cx)
                        });
                        self.loaded = true;
                        self.restart_pending = true;
                    }
                    Err(error) => self.message(format!("无法读取已保存设置：{error:#}"), cx),
                }
            }
            let result = result.and_then(|r| r).and_then(|()| pending.job.result());
            match result {
                Ok(message) => {
                    match &pending.work {
                        Work::Save(generated) => {
                            self.restart_pending = true;
                            if let Some(password) = generated {
                                self.password.update(cx, |input, cx| {
                                    input.set_value(password.to_string(), window, cx)
                                });
                            }
                        }
                        Work::Start | Work::Restart => {
                            self.restart_pending = false;
                            self.reading.request();
                        }
                        Work::Startup => match Preferences::load() {
                            Ok(preferences) => self.preferences = preferences,
                            Err(error) => self.message(format!("{error:#}"), cx),
                        },
                        Work::Uninstall => {
                            self.uninstalling = true;
                        }
                    }
                    self.message(message, cx);
                }
                Err(error) => self.message(format!("{error:#}"), cx),
            }
            if self.uninstalling {
                self.pending = Some(pending);
            } else if let Err(error) = pending.job.close() {
                self.message(format!("{error:#}"), cx);
            }
            if let Some(tray) = &self.tray {
                tray.update(self.preferences, !self.busy());
            }
            self.next_states = Instant::now();
        }
        if let Some(result) = self.reading.poll(management::password_info) {
            match result.and_then(|r| r) {
                Ok(info) => match info.password() {
                    Ok(password) => {
                        self.port.update(cx, |input, cx| {
                            input.set_value(info.port.to_string(), window, cx)
                        });
                        self.password.update(cx, |input, cx| {
                            input.set_value(password.to_string(), window, cx)
                        });
                        self.loaded = true;
                        self.restart_pending = info.pending;
                        self.message(
                            if info.pending {
                                "已保存，下次重启后生效"
                            } else {
                                "后台服务运行中"
                            },
                            cx,
                        );
                    }
                    Err(error) => self.message(format!("{error:#}"), cx),
                },
                Err(error) => self.message(format!("{error:#}"), cx),
            }
        }
        if Instant::now() >= self.next_states && !self.uninstalling {
            self.states_read.request();
            self.next_states = Instant::now() + Duration::from_secs(2);
        }
        if let Some(result) = self.states_read.poll(management::service_states) {
            match result.and_then(|r| r) {
                Ok(states) => {
                    self.states = states;
                    cx.notify();
                }
                Err(error) => {
                    self.states = ["未知".into(), "未知".into()];
                    self.message(format!("读取服务状态失败：{error:#}"), cx);
                }
            }
        }
        while let Some(action) = self.tray.as_ref().and_then(Tray::next) {
            match action {
                Action::Show => {
                    if let Err(error) = tray::show(window) {
                        self.message(format!("{error:#}"), cx);
                    }
                }
                Action::Preferences(preferences) => {
                    self.begin(Request::Startup(preferences), Work::Startup, cx)
                }
                Action::Quit if !self.busy() => cx.quit(),
                Action::Quit => self.message("正在操作，请等待完成", cx),
            }
        }
        if self.preferences.minimize
            && self.tray.is_some()
            && matches!(tray::minimized(window), Ok(true))
            && let Err(error) = tray::hide(window)
        {
            self.message(format!("{error:#}"), cx);
        }
    }
}
impl Render for Panel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy();
        div()
            .v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .v_flex()
                    .p_5()
                    .gap_4()
                    .flex_1()
                    .child(connection_row(
                        &self.port,
                        &self.password,
                        busy || !self.loaded,
                        cx.listener(|this, _, _, cx| this.save(false, cx)),
                        cx.listener(|this, _, _, cx| this.save(true, cx)),
                    ))
                    .children(["通信服务", "桌面服务"].into_iter().zip(&self.states).map(
                        |(name, state)| {
                            div()
                                .h_flex()
                                .items_center()
                                .gap_4()
                                .text_sm()
                                .child(div().w(px(90.)).child(name))
                                .child(state.clone())
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("restart")
                                    .label("重启")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.begin(Request::Restart, Work::Restart, cx)
                                    })),
                            )
                            .child(
                                Button::new("uninstall")
                                    .label("卸载")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.uninstall(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(status_bar(&self.status, cx))
    }
}
pub fn run(startup: bool) -> Result<()> {
    crate::diagnostics::initialize(super::bootstrap::user_logs()?.join("ui.log"))?;
    let title = super::desktop::wide("LanDeskServer 设置");
    let existing = unsafe { FindWindowW(null(), title.as_ptr()) };
    if !existing.is_null() {
        if !startup {
            unsafe {
                ShowWindow(existing, SW_RESTORE);
                SetForegroundWindow(existing);
            }
        }
        return Ok(());
    }
    let preferences = Preferences::load()?;
    let failure = std::rc::Rc::new(std::cell::RefCell::new(None));
    let outcome = failure.clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let (tray, error) = match Tray::new(preferences) {
                Ok(tray) => (Some(tray), None),
                Err(error) => (None, Some(format!("托盘创建失败：{error:#}"))),
            };
            let hidden = startup && preferences.automatic && preferences.minimize && tray.is_some();
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("LanDeskServer 设置".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(620.), px(270.)),
                    cx,
                ))),
                window_min_size: Some(size(px(580.), px(270.))),
                show: !hidden,
                focus: !hidden,
                ..Default::default()
            };
            if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Panel::new(preferences, tray, error, window, cx))
            }) {
                *outcome.borrow_mut() = Some(error);
                cx.quit();
            }
            if !hidden {
                cx.activate(true);
            }
        });
    match failure.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
