use super::management::Preferences;
use anyhow::Result;
use gpui_kit::Window;
use raw_window_handle::HasWindowHandle;
use raw_window_handle::RawWindowHandle;
use tray_icon::Icon;
use tray_icon::MouseButton;
use tray_icon::MouseButtonState;
use tray_icon::TrayIcon;
use tray_icon::TrayIconBuilder;
use tray_icon::TrayIconEvent;
use tray_icon::menu::Menu;
use tray_icon::menu::MenuEvent;
use tray_icon::menu::{CheckMenuItem, MenuItem};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::IsIconic;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_RESTORE;
use windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow;

pub enum Action {
    Show,
    Preferences(Preferences),
    Quit,
}
pub struct Tray {
    _icon: TrayIcon,
    show: MenuItem,
    automatic: CheckMenuItem,
    minimize: CheckMenuItem,
    quit: MenuItem,
}

impl Tray {
    pub fn new(preferences: Preferences) -> Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("打开设置", true, None);
        let automatic = CheckMenuItem::new("开机启动", true, preferences.automatic, None);
        let minimize = CheckMenuItem::new("最小化托盘", true, preferences.minimize, None);
        let quit = MenuItem::new("退出托盘", true, None);
        menu.append_items(&[&show, &automatic, &minimize, &quit])?;
        let pixels = include_bytes!("../../../../../assets/icons/tray.rgba").to_vec();
        let icon = Icon::from_rgba(pixels, 32, 32)?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("LanDeskServer")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;
        Ok(Self {
            _icon: icon,
            show,
            automatic,
            minimize,
            quit,
        })
    }
    pub fn update(&self, preferences: Preferences, enabled: bool) {
        self.automatic.set_checked(preferences.automatic);
        self.minimize.set_checked(preferences.minimize);
        self.automatic.set_enabled(enabled);
        self.minimize.set_enabled(enabled);
        self.quit.set_enabled(enabled);
    }
    pub fn next(&self) -> Option<Action> {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.show.id() {
                return Some(Action::Show);
            }
            if event.id == self.automatic.id() || event.id == self.minimize.id() {
                return Some(Action::Preferences(Preferences {
                    automatic: self.automatic.is_checked(),
                    minimize: self.minimize.is_checked(),
                }));
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

pub fn hwnd(window: &Window) -> Result<HWND> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("无法取得服务端窗口句柄：{error}"))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as HWND),
        _ => anyhow::bail!("服务端窗口不是 Win32 窗口"),
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
