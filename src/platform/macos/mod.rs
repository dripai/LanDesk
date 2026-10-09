pub mod capture;
pub mod files;
use super::enigo_input as input;
mod power;
pub mod ssh;
mod ui;

use super::*;
use anyhow::{Context, ensure};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardTypeString};
use objc2_foundation::{NSArray, NSData};

pub struct MacPlatform;
impl HostPlatform for MacPlatform {
    fn info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: PROTOCOL_VERSION,
            os: "macos",
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
        let (capture, input) = ui::permissions();
        ensure!(
            capture,
            "请在 Mac 系统设置中授予 LanDesk 屏幕录制权限，然后关闭并重新打开应用"
        );
        ensure!(
            input,
            "请在 Mac 系统设置中授予 LanDesk 辅助功能权限，然后关闭并重新打开应用"
        );
        Ok(())
    }
    fn capture(&self) -> Result<Box<dyn CaptureSession>> {
        Ok(Box::new(capture::Capture::start()?))
    }
    fn home_directory(&self) -> Result<PathBuf> {
        Ok(objc2_foundation::NSHomeDirectory().to_string().into())
    }
    fn input(&self, width: i32, height: i32) -> Result<Box<dyn InputController>> {
        Ok(Box::new(input::Input::new(width, height)?))
    }
    fn keep_awake(&self) -> Result<Box<dyn SessionPower>> {
        Ok(Box::new(power::KeepAwake::new()?))
    }
    fn read_clipboard(&self) -> Result<String> {
        let text = NSPasteboard::generalPasteboard()
            .stringForType(unsafe { NSPasteboardTypeString })
            .context("远程剪贴板没有文字；不支持取回图片或文件")?
            .to_string();
        Ok(text)
    }
    fn write_clipboard_image(&self, png: &[u8]) -> Result<()> {
        let item = NSPasteboardItem::new();
        ensure!(
            item.setData_forType(&NSData::with_bytes(png), unsafe { NSPasteboardTypePNG }),
            "无法准备剪贴板图片"
        );
        let objects = NSArray::from_slice(&[ProtocolObject::from_ref(&*item)]);
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        ensure!(pasteboard.writeObjects(&objects), "无法写入图片剪贴板");
        Ok(())
    }
    fn run_ui(&self, control: DesktopControl, commands: mpsc::Receiver<Command>) -> Result<()> {
        ui::run(control, commands)
    }
}

impl SessionPower for power::KeepAwake {}

impl CaptureSession for capture::Capture {
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn input_dimensions(&self) -> (i32, i32) {
        (self.point_width, self.point_height)
    }
    fn frames(&self) -> watch::Receiver<Option<FrameEvent>> {
        self.frames.clone()
    }
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()> {
        self.set_resolution(width)
    }
}
