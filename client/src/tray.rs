use anyhow::{Context, Result};
use gpui_kit::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{IsIconic, SW_HIDE, SW_RESTORE, ShowWindow},
};

pub enum Action {
    Show,
    Disconnect,
    Quit,
}
pub struct Tray {
    _icon: TrayIcon,
    show: MenuItem,
    disconnect: MenuItem,
    quit: MenuItem,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("打开 LanDeskClient", true, None);
        let disconnect = MenuItem::new("断开连接", true, None);
        let quit = MenuItem::new("退出", true, None);
        menu.append_items(&[&show, &disconnect, &quit])?;
        let mut pixels = Vec::with_capacity(32 * 32 * 4);
        for y in 0..32 {
            for x in 0..32 {
                let letter = (8..12).contains(&x) && (7..25).contains(&y)
                    || (8..25).contains(&x) && (21..25).contains(&y);
                pixels.extend_from_slice(if letter {
                    &[255, 255, 255, 255]
                } else {
                    &[38, 114, 237, 255]
                });
            }
        }
        let icon = Icon::from_rgba(pixels, 32, 32)?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("LanDeskClient")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;
        Ok(Self {
            _icon: icon,
            show,
            disconnect,
            quit,
        })
    }
    pub fn next(&self) -> Option<Action> {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.show.id() {
                return Some(Action::Show);
            }
            if event.id == self.disconnect.id() {
                return Some(Action::Disconnect);
            }
            if event.id == self.quit.id() {
                return Some(Action::Quit);
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                return Some(Action::Show);
            }
        }
        None
    }
}

fn hwnd(window: &Window) -> Result<HWND> {
    let handle = HasWindowHandle::window_handle(window).context("无法取得客户端窗口句柄")?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as HWND),
        _ => anyhow::bail!("客户端窗口不是 Win32 窗口"),
    }
}
pub fn hide(window: &Window) -> Result<()> {
    unsafe {
        ShowWindow(hwnd(window)?, SW_HIDE);
    }
    Ok(())
}
pub fn show(window: &Window) -> Result<()> {
    unsafe {
        ShowWindow(hwnd(window)?, SW_RESTORE);
    }
    window.activate_window();
    Ok(())
}
pub fn minimized(window: &Window) -> Result<bool> {
    Ok(unsafe { IsIconic(hwnd(window)?) != 0 })
}
