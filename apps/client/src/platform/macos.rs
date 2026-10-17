use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use gpui_kit::Window;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_app_kit::NSWindow;
use raw_window_handle::HasWindowHandle;
use raw_window_handle::RawWindowHandle;

fn native_window(window: &Window) -> Result<Retained<NSWindow>> {
    ensure!(
        MainThreadMarker::new().is_some(),
        "窗口操作必须在主线程执行"
    );
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("无法取得客户端窗口句柄：{error}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        anyhow::bail!("客户端窗口不是 AppKit 窗口");
    };
    // GPUI owns this view for the lifetime of the borrowed Window.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window().context("客户端原生窗口已关闭")
}

pub fn hide(window: &Window) -> Result<()> {
    native_window(window)?.orderOut(None);
    Ok(())
}
pub fn show(window: &Window) -> Result<()> {
    let native = native_window(window)?;
    if native.isMiniaturized() {
        native.deminiaturize(None);
    }
    window.activate_window();
    Ok(())
}
