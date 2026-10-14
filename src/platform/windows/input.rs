use super::{
    capture,
    desktop::{Desktop, checked},
    service,
};
use crate::platform::InputEvent;
use crate::{
    platform::{Capabilities, DesktopControl, FileBackend, FileSession, Shared},
    protocol::{MAX_TEXT_BYTES, pointer_position},
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::future::BoxFuture;
use std::{
    collections::BTreeSet,
    mem::size_of,
    ptr::null_mut,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use windows_sys::Win32::{
    System::{DataExchange::*, Memory::*},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

fn send(input: &[INPUT]) -> Result<()> {
    unsafe {
        windows_sys::Win32::Foundation::SetLastError(0);
    }
    let sent = unsafe {
        SendInput(
            input.len() as u32,
            input.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    ensure!(
        sent == input.len() as u32,
        "Windows 拒绝键鼠注入：成功 {sent}/{}，{}",
        input.len(),
        if error == 0 {
            "系统未提供错误原因".into()
        } else {
            std::io::Error::from_raw_os_error(error as i32).to_string()
        }
    );
    Ok(())
}
fn key_input(key: u16, down: bool, unicode: bool) -> INPUT {
    let extended = !unicode
        && matches!(
            key,
            VK_LEFT
                | VK_RIGHT
                | VK_UP
                | VK_DOWN
                | VK_HOME
                | VK_END
                | VK_PRIOR
                | VK_NEXT
                | VK_INSERT
                | VK_DELETE
                | VK_LWIN
        );
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: if unicode { 0 } else { key },
                wScan: if unicode { key } else { 0 },
                dwFlags: (if down { 0 } else { KEYEVENTF_KEYUP })
                    | (if unicode { KEYEVENTF_UNICODE } else { 0 })
                    | (if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }),
                ..Default::default()
            },
        },
    }
}
fn mouse(flags: u32, x: i32, y: i32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x,
                dy: y,
                mouseData: data,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}
fn key_code(key: &str) -> Result<u16> {
    Ok(match key {
        "Control" => VK_CONTROL,
        "Meta" => VK_LWIN,
        "Alt" => VK_MENU,
        "Shift" => VK_SHIFT,
        "Enter" => VK_RETURN,
        "Escape" => VK_ESCAPE,
        "Tab" => VK_TAB,
        "Backspace" => VK_BACK,
        "Delete" => VK_DELETE,
        "ArrowLeft" => VK_LEFT,
        "ArrowRight" => VK_RIGHT,
        "ArrowUp" => VK_UP,
        "ArrowDown" => VK_DOWN,
        "Home" => VK_HOME,
        "End" => VK_END,
        "PageUp" => VK_PRIOR,
        "PageDown" => VK_NEXT,
        "F1" => VK_F1,
        "F2" => VK_F2,
        "F3" => VK_F3,
        "F4" => VK_F4,
        "F5" => VK_F5,
        "F6" => VK_F6,
        "F7" => VK_F7,
        "F8" => VK_F8,
        "F9" => VK_F9,
        "F10" => VK_F10,
        "F11" => VK_F11,
        "F12" => VK_F12,
        value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => {
            u16::from(value.as_bytes()[0].to_ascii_uppercase())
        }
        value if value.encode_utf16().count() == 1 => {
            let code = unsafe { VkKeyScanW(value.encode_utf16().next().unwrap()) };
            ensure!(code != -1, "当前键盘布局不支持此快捷键");
            code as u16 & 0xff
        }
        _ => bail!("不支持的 Windows 按键"),
    })
}
fn button(button: u8, down: bool) -> Result<u32> {
    Ok(match (button, down) {
        (0, true) => MOUSEEVENTF_LEFTDOWN,
        (0, false) => MOUSEEVENTF_LEFTUP,
        (1, true) => MOUSEEVENTF_MIDDLEDOWN,
        (1, false) => MOUSEEVENTF_MIDDLEUP,
        (2, true) => MOUSEEVENTF_RIGHTDOWN,
        (2, false) => MOUSEEVENTF_RIGHTUP,
        _ => bail!("不支持的鼠标按钮"),
    })
}
struct Input {
    desktop: Desktop,
    shared: Arc<Shared>,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
}
impl Input {
    fn release(&mut self) -> Result<()> {
        let mut events = Vec::new();
        for key in &self.keys {
            events.push(key_input(*key, false, false));
        }
        for b in &self.buttons {
            events.push(mouse(button(*b, false)?, 0, 0, 0));
        }
        if !events.is_empty() {
            send(&events)?;
        }
        self.keys.clear();
        self.buttons.clear();
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<()> {
        ensure!(
            text.len() <= MAX_TEXT_BYTES && !text.contains('\0'),
            "文字超过 64 KiB 或包含空字符"
        );
        self.release()?;
        // Unicode packets support Chinese and password fields without relying
        // on the remote keyboard layout or replacing the user's clipboard.
        for units in text.encode_utf16().collect::<Vec<_>>().chunks(128) {
            let events: Vec<_> = units
                .iter()
                .flat_map(|unit| [key_input(*unit, true, true), key_input(*unit, false, true)])
                .collect();
            send(&events)?;
        }
        Ok(())
    }
    fn apply(&mut self, message: InputEvent) -> Result<()> {
        if self.desktop.sync()? {
            self.release()?;
        }
        match message {
            InputEvent::ReleaseAll => self.release(),
            InputEvent::Pointer { x, y } => {
                let m = capture::monitor(self.shared.display.load(Ordering::Acquire))?;
                let (x, y) = pointer_position(x, y, m.width as i32, m.height as i32)?;
                unsafe {
                    let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
                    let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
                    let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                    let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                    ensure!(width > 1 && height > 1, "虚拟桌面尺寸无效");
                    let x = (i64::from(x + m.x - left) * 65535 / i64::from(width - 1)) as i32;
                    let y = (i64::from(y + m.y - top) * 65535 / i64::from(height - 1)) as i32;
                    send(&[mouse(
                        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                        x,
                        y,
                        0,
                    )])
                }
            }
            InputEvent::Button { button: b, down } => {
                // Track before injecting: even a partial SendInput must be released.
                let flags = button(b, down)?;
                if down {
                    self.buttons.insert(b);
                }
                send(&[mouse(flags, 0, 0, 0)])?;
                if !down {
                    self.buttons.remove(&b);
                }
                Ok(())
            }
            InputEvent::Wheel { x, y } => {
                ensure!(
                    (-10..=10).contains(&x) && (-10..=10).contains(&y),
                    "滚动幅度无效"
                );
                if y != 0 {
                    send(&[mouse(MOUSEEVENTF_WHEEL, 0, 0, (-y * 120) as u32)])?;
                }
                if x != 0 {
                    send(&[mouse(MOUSEEVENTF_HWHEEL, 0, 0, (x * 120) as u32)])?;
                }
                Ok(())
            }
            InputEvent::Key { key, down } => {
                let code = key_code(&key)?;
                if down {
                    self.keys.insert(code);
                }
                send(&[key_input(code, down, false)])?;
                if !down {
                    self.keys.remove(&code);
                }
                Ok(())
            }
            InputEvent::Text { text } => self.text(&text),
        }
    }
    fn clipboard(&mut self, copy: bool) -> Result<String> {
        self.desktop.sync()?;
        if copy {
            self.release()?;
            let sequence = unsafe { GetClipboardSequenceNumber() };
            let pressed = (|| {
                self.apply(InputEvent::Key {
                    key: "Control".into(),
                    down: true,
                })?;
                self.apply(InputEvent::Key {
                    key: "c".into(),
                    down: true,
                })
            })();
            let released = self.release();
            pressed?;
            released?;
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            loop {
                // GetClipboardData also asks applications to render delayed
                // clipboard data; polling only the sequence can never do that.
                let text = self.clipboard(false);
                if unsafe { GetClipboardSequenceNumber() } != sequence {
                    return text;
                }
                ensure!(
                    std::time::Instant::now() < deadline,
                    "远程应用未更新剪贴板，请先选中文字再复制"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        unsafe {
            checked(OpenClipboard(null_mut()), "打开 Windows 剪贴板")?;
            struct Open;
            impl Drop for Open {
                fn drop(&mut self) {
                    unsafe {
                        CloseClipboard();
                    }
                }
            }
            let _open = Open;
            let handle = GetClipboardData(13 /* CF_UNICODETEXT */);
            ensure!(!handle.is_null(), "剪贴板中没有文字");
            let bytes = GlobalSize(handle);
            ensure!(
                (2..=2 * MAX_TEXT_BYTES + 2).contains(&bytes) && bytes.is_multiple_of(2),
                "剪贴板文字过大或无效"
            );
            let pointer = GlobalLock(handle);
            ensure!(!pointer.is_null(), "读取剪贴板内存失败");
            let data = std::slice::from_raw_parts(pointer.cast::<u16>(), bytes / 2);
            let text = data
                .iter()
                .position(|c| *c == 0)
                .context("剪贴板文字缺少结束标记")
                .and_then(|len| String::from_utf16(&data[..len]).context("剪贴板文字编码无效"));
            GlobalUnlock(handle);
            let text = text?;
            ensure!(text.len() <= MAX_TEXT_BYTES, "剪贴板文字超过 64 KiB");
            Ok(text)
        }
    }
}
enum Command {
    Input(InputEvent),
    Clipboard(bool),
}
struct Job {
    command: Command,
    reply: tokio::sync::oneshot::Sender<Result<String>>,
}
pub struct WindowsControl {
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<Job>,
    restore: Arc<AtomicBool>,
    worker: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl WindowsControl {
    pub fn new(shared: Arc<Shared>) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(32);
        let restore = Arc::new(AtomicBool::new(false));
        let release = restore.clone();
        let state = shared.clone();
        let worker = std::thread::Builder::new()
            .name("landesk-input".into())
            .spawn(move || {
                let mut input = Input {
                    desktop: Desktop::new(),
                    shared: state.clone(),
                    keys: BTreeSet::new(),
                    buttons: BTreeSet::new(),
                };
                let mut last_failure = None;
                loop {
                    if release.swap(false, Ordering::AcqRel)
                        && input.apply(InputEvent::ReleaseAll).is_err()
                    {
                        release.store(true, Ordering::Release);
                    }
                    if state.shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    match receiver.recv_timeout(Duration::from_millis(50)) {
                        Ok(job) => {
                            if job.reply.is_closed() {
                                continue;
                            }
                            let can_recover = match &job.command {
                                Command::Clipboard(false) => false,
                                Command::Input(InputEvent::ReleaseAll) => false,
                                Command::Input(InputEvent::Text { text }) => !text.is_empty(),
                                Command::Input(InputEvent::Wheel { x, y }) => *x != 0 || *y != 0,
                                _ => true,
                            };
                            let result = match job.command {
                                Command::Input(message) => {
                                    input.apply(message).map(|()| String::new())
                                }
                                Command::Clipboard(copy) => input.clipboard(copy),
                            };
                            if let Err(error) = &result {
                                if !input.keys.is_empty() || !input.buttons.is_empty() {
                                    release.store(true, Ordering::Release);
                                }
                                let message = format!("{error:#}");
                                if last_failure.is_none() {
                                    service::log(&format!(
                                        "Input/clipboard operation failed: {message}"
                                    ));
                                    service::log(&format!(
                                        "Input failure desktop: {}; console state: {}",
                                        super::desktop::context(),
                                        super::interactive::report()
                                    ));
                                    last_failure = Some(message);
                                }
                            } else if can_recover && last_failure.take().is_some() {
                                service::log("Input/clipboard operation recovered");
                            }
                            let _ = job.reply.send(result);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                if let Err(error) = input.apply(InputEvent::ReleaseAll) {
                    service::log(&format!("Release input: {error:#}"));
                }
            })?;
        Ok(Self {
            shared,
            sender,
            restore,
            worker: std::sync::Mutex::new(Some(worker)),
        })
    }
    fn call(&self, command: Command) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let (reply, result) = tokio::sync::oneshot::channel();
            self.sender
                .try_send(Job { command, reply })
                .map_err(|_| anyhow::anyhow!("Windows 输入队列已满或已停止"))?;
            tokio::time::timeout(Duration::from_secs(5), result)
                .await
                .context("Windows 输入操作超时")?
                .context("Windows 输入线程已停止")?
        })
    }
}
impl Drop for WindowsControl {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}
impl DesktopControl for WindowsControl {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            os: "windows",
            virtual_display: true,
        }
    }
    fn supports_files(&self) -> bool {
        false
    }
    fn check_permissions(&self) -> Result<()> {
        service::require_system_console()
    }
    fn begin_input(&self, display_id: u32) -> BoxFuture<'_, Result<()>> {
        self.shared.display.store(display_id, Ordering::Release);
        self.input(InputEvent::ReleaseAll)
    }
    fn input(&self, message: InputEvent) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.call(Command::Input(message)).await.map(|_| ()) })
    }
    fn paste_text(&self, text: String) -> BoxFuture<'_, Result<()>> {
        self.input(InputEvent::Text { text })
    }
    fn paste_image(&self, _png: Vec<u8>) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { bail!("Windows 暂未接入图片剪贴板，请使用文字输入") })
    }
    fn copy_text(&self) -> BoxFuture<'_, Result<String>> {
        self.call(Command::Clipboard(true))
    }
    fn read_clipboard(&self) -> BoxFuture<'_, Result<String>> {
        self.call(Command::Clipboard(false))
    }
    fn end_session(&self) -> BoxFuture<'_, Result<()>> {
        self.input(InputEvent::ReleaseAll)
    }
    fn restore(&self) {
        self.restore.store(true, Ordering::Release);
    }
    fn secure_attention(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { service::request_sas() })
    }
}

// Never expose LocalSystem's filesystem through the user's file panel.
pub struct NoFiles;
impl FileBackend for NoFiles {
    fn open_session(self: Arc<Self>, _: Arc<AtomicBool>) -> Box<dyn FileSession> {
        Box::new(NoFiles)
    }
}
impl FileSession for NoFiles {
    fn message(&mut self, message: crate::platform::FileCommand) -> crate::platform::FileResponse {
        crate::platform::FileResponse::FileError {
            id: message.id(),
            message: "Windows 桌面服务不提供 SYSTEM 文件访问".into(),
            upload_active: false,
        }
    }
    fn chunk(&mut self, _: &[u8]) -> crate::platform::FileResponse {
        crate::platform::FileResponse::FileError {
            id: 0,
            message: "Windows 桌面服务不提供文件上传".into(),
            upload_active: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_shortcuts_keep_control_and_windows_keys() {
        assert_eq!(key_code("Control").unwrap(), VK_CONTROL);
        assert_eq!(key_code("Meta").unwrap(), VK_LWIN);
        assert_eq!(key_code("c").unwrap(), b'C' as u16);
        assert!(key_code("Unknown").is_err());
    }
}
