use super::native::{self, Command, Native};
use crate::bootstrap::macos::TransportUpdate;
use crate::settings_view::{connection_row, input, status_bar};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::component::button::*;
use gpui_kit::component::input::InputState;
use gpui_kit::component::*;
use gpui_kit::*;
use landesk_core::application::{operation::Operation, password::PasswordOutcome};
use std::sync::{Arc, atomic::Ordering, mpsc};
use std::time::Duration;
use zeroize::Zeroizing;

actions!(landesk_server, [Quit]);
struct SavedSettings {
    port: u16,
    warning: Option<String>,
    generated_password: Option<Zeroizing<String>>,
}
struct Panel {
    port: Entity<InputState>,
    password: Entity<InputState>,
    current_port: u16,
    shared: Arc<landesk_core::application::runtime::Shared>,
    updates: tokio::sync::mpsc::UnboundedSender<TransportUpdate>,
    saving: Operation<Result<SavedSettings>>,
    status: String,
    permissions: (bool, bool),
    active: bool,
    quitting: bool,
    _timer: super::run_loop::RunLoopTimer,
}
impl Panel {
    fn new(
        native: Native,
        commands: mpsc::Receiver<Command>,
        port: u16,
        password: Result<Zeroizing<String>>,
        updates: tokio::sync::mpsc::UnboundedSender<TransportUpdate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (password, status) = match password {
            Ok(password) => (password.to_string(), "等待连接".into()),
            Err(error) => (String::new(), format!("{error:#}；请设置并保存连接密码")),
        };
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |this, cx| this.quit(cx));
            false
        });
        let weak = cx.weak_entity();
        App::on_action(cx, move |_: &Quit, cx| {
            let _ = weak.update(cx, |this, cx| this.quit(cx));
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
            port: input(port.to_string(), window, cx),
            password: input(password, window, cx),
            current_port: port,
            shared: native.shared.clone(),
            updates,
            saving: Operation::new(),
            status,
            permissions: native::permissions(),
            active: false,
            quitting: false,
            _timer: native::command_timer(native, commands),
        }
    }
    fn quit(&mut self, cx: &mut Context<Self>) {
        self.quitting = true;
        if !self.saving.is_busy() {
            self.shared.shutdown.store(true, Ordering::Release);
            cx.quit();
        } else {
            self.status = "正在保存，完成后退出…".into();
            cx.notify();
        }
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        let result = (|| -> Result<()> {
            ensure!(!self.saving.is_busy(), "正在保存…");
            let port: u16 = self
                .port
                .read(cx)
                .value()
                .trim()
                .parse()
                .context("端口必须在 1–65535 之间")?;
            ensure!(port != 0, "端口必须在 1–65535 之间");
            let password = Zeroizing::new(self.password.read(cx).value().to_string());
            landesk_core::domain::password::validate(&password)?;
            let current_port = self.current_port;
            let updates = self.updates.clone();
            self.saving
                .start(move || save_settings(current_port, port, password, updates))?;
            Ok(())
        })();
        self.status = match result {
            Ok(()) => "正在保存…".into(),
            Err(error) => format!("{error:#}"),
        };
        cx.notify();
    }
    fn refresh_password(&mut self, cx: &mut Context<Self>) {
        let result = (|| -> Result<()> {
            ensure!(!self.saving.is_busy(), "正在保存…");
            let password = landesk_core::domain::password::generate()?;
            let port = self.current_port;
            let updates = self.updates.clone();
            self.saving.start(move || {
                let mut saved = save_settings(port, port, password.clone(), updates)?;
                saved.generated_password = Some(password);
                Ok(saved)
            })?;
            Ok(())
        })();
        self.status = match result {
            Ok(()) => "正在更新密码…".into(),
            Err(error) => format!("{error:#}"),
        };
        cx.notify();
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(result) = self.saving.poll() {
            self.status = match result.and_then(|result| result) {
                Ok(saved) => {
                    self.current_port = saved.port;
                    if let Some(password) = saved.generated_password {
                        self.password.update(cx, |input, cx| {
                            input.set_value(password.to_string(), window, cx);
                        });
                        saved.warning.unwrap_or_else(|| "密码已更新并保存".into())
                    } else {
                        saved.warning.unwrap_or_else(|| "已保存".into())
                    }
                }
                Err(error) => format!("{error:#}"),
            };
            cx.notify();
        }
        let permissions = native::permissions();
        if permissions != self.permissions {
            self.permissions = permissions;
            self.status = "授权状态已更新".into();
            cx.notify();
        }
        let active = self.shared.active.load(Ordering::Acquire);
        if active != self.active {
            self.active = active;
            self.status = if active {
                "远程已连接"
            } else {
                "连接已断开，等待连接"
            }
            .into();
            cx.notify();
        }
        if self.quitting && !self.saving.is_busy() {
            self.quit(cx);
        } else if self.shared.shutdown.load(Ordering::Acquire) {
            self.status = "后台服务已停止，请关闭应用后重新打开".into();
            cx.notify();
        }
    }
}
impl Render for Panel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.saving.is_busy() || self.quitting;
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
                        disabled,
                        cx.listener(|this, _, _, cx| this.save(cx)),
                        cx.listener(|this, _, _, cx| this.refresh_password(cx)),
                    ))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().child("授权"))
                            .child(
                                Button::new("capture")
                                    .label("屏幕录制")
                                    .disabled(self.permissions.0)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        native::request_capture();
                                        this.status = "已请求屏幕录制授权".into();
                                        cx.notify();
                                    })),
                            )
                            .child(div().w(px(24.)).child(if self.permissions.0 {
                                "✓"
                            } else {
                                ""
                            }))
                            .child(
                                Button::new("input")
                                    .label("辅助功能")
                                    .disabled(self.permissions.1)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.status = match native::request_input() {
                                            Ok(()) => "已请求辅助功能授权".into(),
                                            Err(error) => format!("{error:#}"),
                                        };
                                        cx.notify();
                                    })),
                            )
                            .child(div().w(px(24.)).child(if self.permissions.1 {
                                "✓"
                            } else {
                                ""
                            })),
                    ),
            )
            .child(status_bar(&self.status, cx))
    }
}

fn save_settings(
    current_port: u16,
    port: u16,
    password: Zeroizing<String>,
    updates: tokio::sync::mpsc::UnboundedSender<TransportUpdate>,
) -> Result<SavedSettings> {
    // Reserve the new port before changing either persisted setting.
    let listener = if port != current_port {
        let listener =
            std::net::TcpListener::bind(("0.0.0.0", port)).context("端口不可用，配置未修改")?;
        listener.set_nonblocking(true)?;
        Some(listener)
    } else {
        None
    };
    let path = super::connection::path()?;
    let outcome = landesk_core::application::password::change_connection(
        &crate::adapters::identity::IdentityFile(path.clone()),
        &landesk_storage::LocalSecrets::beside(&path),
        password,
        port,
    );
    let warning = match outcome {
        PasswordOutcome::Saved { warning, .. } => warning,
        PasswordOutcome::Failed(error) => return Err(error.into()),
    };
    if let Some(listener) = listener {
        let identity =
            crate::adapters::identity::read(&path).context("配置已保存，但无法重新读取")?;
        let (ready, result) = mpsc::channel();
        updates
            .send(TransportUpdate {
                listener,
                identity,
                ready,
            })
            .map_err(|_| anyhow::anyhow!("配置已保存，但后台服务已停止，请重新启动"))?;
        result
            .recv_timeout(Duration::from_secs(8))
            .context("配置已保存，但监听切换尚未确认，请重新启动")?;
    }
    Ok(SavedSettings {
        port,
        warning,
        generated_password: None,
    })
}
pub fn run(
    native: Native,
    commands: mpsc::Receiver<Command>,
    port: u16,
    password: Result<Zeroizing<String>>,
    updates: tokio::sync::mpsc::UnboundedSender<TransportUpdate>,
) -> Result<()> {
    let failure = std::rc::Rc::new(std::cell::RefCell::new(None));
    let result = failure.clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus([Menu::new("LanDesk").items([MenuItem::action("退出 LanDesk", Quit)])]);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("LanDesk".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(600.), px(190.)),
                    cx,
                ))),
                window_min_size: Some(size(px(560.), px(190.))),
                ..Default::default()
            };
            if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Panel::new(native, commands, port, password, updates, window, cx))
            }) {
                *failure.borrow_mut() = Some(error);
                cx.quit();
            }
            cx.activate(true);
        });
    match result.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Zeroizing, save_settings};
    #[test]
    fn occupied_port_fails_before_reading_or_changing_configuration() {
        let occupied = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let (updates, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let error = save_settings(0, port, Zeroizing::new("test".into()), updates)
            .err()
            .unwrap();
        assert!(error.to_string().contains("端口不可用，配置未修改"));
        assert!(receiver.try_recv().is_err());
    }
}
