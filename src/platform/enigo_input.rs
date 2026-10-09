use crate::protocol::{ClientMessage, MAX_TEXT_BYTES, pointer_position};
use anyhow::{Result, bail, ensure};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::collections::HashSet;

pub struct Input {
    enigo: Enigo,
    keys: HashSet<Key>,
    buttons: HashSet<Button>,
    width: i32,
    height: i32,
}

fn map_key(key: &str) -> Result<Key> {
    Ok(match key {
        "Control" => {
            if cfg!(target_os = "macos") {
                Key::Meta
            } else {
                Key::Control
            }
        }
        "Meta" => {
            if cfg!(target_os = "macos") {
                Key::Control
            } else {
                Key::Meta
            }
        }
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
    pub fn paste(&mut self) -> Result<()> {
        self.release_all()?;
        let modifier = if cfg!(target_os = "macos") {
            Key::Meta
        } else {
            Key::Control
        };
        self.enigo.key(modifier, Direction::Press)?;
        let paste = self.enigo.key(Key::Unicode('v'), Direction::Click);
        let release = self.enigo.key(modifier, Direction::Release);
        paste?;
        release?;
        Ok(())
    }
    pub fn new(width: i32, height: i32) -> Result<Self> {
        #[cfg(target_os = "macos")]
        ensure!(
            objc2::MainThreadMarker::new().is_some(),
            "键鼠控制必须在主线程启动"
        );
        let settings = Settings {
            open_prompt_to_get_permissions: false,
            release_keys_when_dropped: true,
            ..Settings::default()
        };
        let enigo = Enigo::new(&settings)
            .map_err(|e| anyhow::anyhow!("无法控制键鼠，请检查当前桌面会话及控制权限: {e}"))?;
        Ok(Self {
            enigo,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            width,
            height,
        })
    }
    pub fn handle(&mut self, message: ClientMessage) -> Result<()> {
        #[cfg(target_os = "macos")]
        ensure!(
            objc2::MainThreadMarker::new().is_some(),
            "键鼠操作必须在主线程执行"
        );
        match message {
            ClientMessage::Pointer { x, y } => {
                let (x, y) = pointer_position(x, y, self.width, self.height)?;
                self.enigo.move_mouse(x, y, Coordinate::Abs)?;
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

use crate::platform::InputController;
impl InputController for Input {
    fn handle(&mut self, message: ClientMessage) -> Result<()> {
        self.handle(message)
    }
    fn paste(&mut self) -> Result<()> {
        self.paste()
    }
    fn release_all(&mut self) -> Result<()> {
        self.release_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_shortcuts_preserve_control() {
        assert_eq!(map_key("Control").unwrap(), Key::Control);
        assert_eq!(map_key("Meta").unwrap(), Key::Meta);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn worker_thread_is_rejected_before_native_input_apis() {
        let error = match Input::new(100, 100) {
            Ok(_) => panic!("worker thread must not create native input"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("主线程"));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn windows_shortcuts_map_to_mac_command() {
        assert_eq!(map_key("Control").unwrap(), Key::Meta);
        assert_eq!(map_key("Enter").unwrap(), Key::Return);
        assert!(map_key("run arbitrary command").is_err());
    }
}
