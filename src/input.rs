use crate::protocol::{ClientMessage, MAX_TEXT_BYTES, pointer_position};
use anyhow::{Result, bail, ensure};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::collections::HashSet;

pub struct Input {
    enigo: Enigo,
    keys: HashSet<Key>,
    buttons: HashSet<Button>,
    display: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

fn map_key(key: &str) -> Result<Key> {
    Ok(match key {
        "Control" => Key::Meta, // Windows Ctrl shortcuts become Mac Command shortcuts.
        "Meta" => Key::Control,
        "Alt" => Key::Alt,
        "Shift" => Key::Shift,
        "Enter" => Key::Return,
        "Escape" => Key::Escape,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        s if s.chars().count() == 1 => Key::Unicode(s.chars().next().unwrap()),
        _ => bail!("暂不支持按键: {key}"),
    })
}

impl Input {
    pub fn copy(&mut self) -> Result<()> {
        self.shortcut('c')
    }
    pub fn paste(&mut self) -> Result<()> {
        self.shortcut('v')
    }
    fn shortcut(&mut self, key: char) -> Result<()> {
        self.release_all()?;
        self.enigo.key(Key::Meta, Direction::Press)?;
        let paste = self.enigo.key(Key::Unicode(key), Direction::Click);
        let release = self.enigo.key(Key::Meta, Direction::Release);
        paste?;
        release?;
        Ok(())
    }
    pub fn new(display: std::sync::Arc<std::sync::atomic::AtomicU32>) -> Result<Self> {
        ensure!(
            objc2::MainThreadMarker::new().is_some(),
            "键鼠控制必须在主线程启动"
        );
        let settings = Settings {
            open_prompt_to_get_permissions: false,
            release_keys_when_dropped: true,
            ..Settings::default()
        };
        let enigo = Enigo::new(&settings).map_err(|e| {
            anyhow::anyhow!("无法控制键鼠，请授予 LanDesk 辅助功能权限并重启应用: {e}")
        })?;
        Ok(Self {
            enigo,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            display,
        })
    }
    pub fn handle(&mut self, message: ClientMessage) -> Result<()> {
        ensure!(
            objc2::MainThreadMarker::new().is_some(),
            "键鼠操作必须在主线程执行"
        );
        match message {
            ClientMessage::Pointer { x, y } => {
                let bounds = crate::capture::display_bounds(
                    self.display.load(std::sync::atomic::Ordering::Acquire),
                );
                let (x, y) = pointer_position(
                    x,
                    y,
                    bounds.size.width.round() as i32,
                    bounds.size.height.round() as i32,
                )?;
                self.enigo.move_mouse(
                    x + bounds.origin.x.round() as i32,
                    y + bounds.origin.y.round() as i32,
                    Coordinate::Abs,
                )?;
            }
            ClientMessage::Button { button, down } => {
                let button = match button {
                    0 => Button::Left,
                    1 => Button::Middle,
                    2 => Button::Right,
                    _ => bail!("不支持的鼠标按钮"),
                };
                self.enigo.button(
                    button,
                    if down {
                        Direction::Press
                    } else {
                        Direction::Release
                    },
                )?;
                if down {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(&button);
                }
            }
            ClientMessage::Wheel { x, y } => {
                ensure!(
                    x.unsigned_abs() <= 100 && y.unsigned_abs() <= 100,
                    "滚动幅度过大"
                );
                if x != 0 {
                    self.enigo.scroll(x, Axis::Horizontal)?;
                }
                if y != 0 {
                    self.enigo.scroll(y, Axis::Vertical)?;
                }
            }
            ClientMessage::Key { key, down } => {
                let key = map_key(&key)?;
                self.enigo.key(
                    key,
                    if down {
                        Direction::Press
                    } else {
                        Direction::Release
                    },
                )?;
                if down {
                    self.keys.insert(key);
                } else {
                    self.keys.remove(&key);
                }
            }
            ClientMessage::Text { text } => {
                ensure!(text.len() <= MAX_TEXT_BYTES, "文本不能超过 64 KiB");
                ensure!(!text.contains('\0'), "文本不能包含空字符");
                self.enigo.text(&text)?;
            }
            ClientMessage::ReleaseAll => self.release_all()?,
            _ => bail!("不是键鼠消息"),
        }
        Ok(())
    }
    pub fn release_all(&mut self) -> Result<()> {
        let mut failure = None;
        for key in self.keys.drain() {
            if let Err(e) = self.enigo.key(key, Direction::Release) {
                failure = Some(e);
            }
        }
        for button in self.buttons.drain() {
            if let Err(e) = self.enigo.button(button, Direction::Release) {
                failure = Some(e);
            }
        }
        if let Some(e) = failure {
            return Err(e.into());
        }
        Ok(())
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        if let Err(e) = self.release_all() {
            eprintln!("释放键鼠失败: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_thread_is_rejected_before_native_input_apis() {
        let error = match Input::new(std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1))) {
            Ok(_) => panic!("worker thread must not create native input"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("主线程"));
    }
    #[test]
    fn windows_shortcuts_map_to_mac_command() {
        assert_eq!(map_key("Control").unwrap(), Key::Meta);
        assert_eq!(map_key("Enter").unwrap(), Key::Return);
        assert!(map_key("run arbitrary command").is_err());
    }
}
