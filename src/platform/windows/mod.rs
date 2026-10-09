mod capture;
pub mod files;
mod ui;

use super::*;
use anyhow::{Context, ensure};
use std::borrow::Cow;
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};

pub struct WindowsPlatform;
impl HostPlatform for WindowsPlatform {
    fn info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: PROTOCOL_VERSION,
            os: "windows",
            capabilities: Capabilities {
                capture: true,
                input: true,
                clipboard_text: true,
                clipboard_image: true,
                files: true,
                capture_resize: true,
                display_sleep: false,
            },
        }
    }
    fn check_permissions(&self) -> Result<()> {
        capture::check_desktop()
    }
    fn capture(&self) -> Result<Box<dyn CaptureSession>> {
        Ok(Box::new(capture::Capture::start()?))
    }
    fn home_directory(&self) -> Result<PathBuf> {
        Ok(std::env::var_os("USERPROFILE")
            .context("无法读取当前用户目录")?
            .into())
    }
    fn input(&self, width: i32, height: i32) -> Result<Box<dyn InputController>> {
        capture::check_desktop()?;
        Ok(Box::new(super::enigo_input::Input::new(width, height)?))
    }
    fn keep_awake(&self) -> Result<Box<dyn SessionPower>> {
        ensure!(
            unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) } != 0,
            "无法阻止远控期间系统自动休眠"
        );
        Ok(Box::new(KeepAwake))
    }
    fn read_clipboard(&self) -> Result<String> {
        Ok(arboard::Clipboard::new()?.get_text()?)
    }
    fn write_clipboard_image(&self, png: &[u8]) -> Result<()> {
        let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)?.into_rgba8();
        let (width, height) = image.dimensions();
        arboard::Clipboard::new()?.set_image(arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: Cow::Owned(image.into_raw()),
        })?;
        Ok(())
    }
    fn run_ui(&self, control: DesktopControl, commands: mpsc::Receiver<Command>) -> Result<()> {
        ui::run(control, commands)
    }
}
struct KeepAwake;
impl SessionPower for KeepAwake {}
impl Drop for KeepAwake {
    fn drop(&mut self) {
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}
