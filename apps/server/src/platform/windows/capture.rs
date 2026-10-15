use super::desktop::Desktop;
use super::desktop::checked;
use super::desktop::wide;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use image::codecs::jpeg::JpegEncoder;
use landesk_core::domain::CaptureInfo;
use landesk_core::domain::DisplayInfo;
use landesk_core::domain::DisplayTarget;
use landesk_core::domain::FrameEvent;
use landesk_core::ports::CaptureBackend;
use landesk_core::ports::FrameSource;
use std::mem::size_of;
use std::ptr::null;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::watch;
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Debug)]
pub struct Monitor {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    main: bool,
    virtual_display: bool,
}
fn string(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())])
}
pub fn monitors() -> Result<Vec<Monitor>> {
    let mut desktop = Desktop::new();
    desktop.sync()?;
    let virtual_names = virtual_names()?;
    let mut result = Vec::new();
    for index in 0..128 {
        let mut device = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        if unsafe { EnumDisplayDevicesW(null(), index, &mut device, 0) } == 0 {
            break;
        }
        if device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0 {
            continue;
        }
        let name = string(&device.DeviceName);
        let id: u32 = name
            .strip_prefix(r"\\.\DISPLAY")
            .context("无法识别显示器名称")?
            .parse()?;
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        checked(
            unsafe {
                EnumDisplaySettingsW(device.DeviceName.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode)
            },
            "读取显示器模式",
        )?;
        let position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
        ensure!(
            mode.dmPelsWidth > 0 && mode.dmPelsHeight > 0,
            "显示器尺寸无效"
        );
        result.push(Monitor {
            id,
            x: position.x,
            y: position.y,
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            main: device.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0,
            virtual_display: virtual_names.contains(&name),
        });
    }
    ensure!(
        !result.is_empty(),
        "没有活动显示器，请检查内置虚拟显示驱动状态"
    );
    Ok(result)
}
pub fn monitor(id: u32) -> Result<Monitor> {
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    checked(
        unsafe {
            EnumDisplaySettingsW(
                wide(&format!(r"\\.\DISPLAY{id}")).as_ptr(),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
        },
        "读取选定显示器",
    )?;
    ensure!(
        mode.dmPelsWidth > 0 && mode.dmPelsHeight > 0,
        "选定显示器已不可用"
    );
    let position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
    Ok(Monitor {
        id,
        x: position.x,
        y: position.y,
        width: mode.dmPelsWidth,
        height: mode.dmPelsHeight,
        main: false,
        virtual_display: false,
    })
}

fn virtual_names() -> Result<Vec<String>> {
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0, 0);
        let result = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        ensure!(result == 0, "读取显示拓扑大小失败: {result}");
        ensure!(
            path_count <= 128 && mode_count <= 512,
            "显示拓扑超过支持范围"
        );
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                null_mut(),
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        ensure!(result == 0, "读取显示拓扑失败: {result}");
        let mut names = Vec::new();
        for path in paths.iter().take(path_count as usize) {
            let mut adapter = DISPLAYCONFIG_ADAPTER_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
                    size: size_of::<DISPLAYCONFIG_ADAPTER_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: 0,
                },
                ..Default::default()
            };
            let result = unsafe { DisplayConfigGetDeviceInfo(&mut adapter.header) };
            ensure!(result == 0, "读取显示适配器失败: {result}");
            if !super::virtual_display::is_mtt_adapter(&adapter.adapterDevicePath)? {
                continue;
            }
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            let result = unsafe { DisplayConfigGetDeviceInfo(&mut source.header) };
            ensure!(result == 0, "读取虚拟显示器名称失败: {result}");
            names.push(string(&source.viewGdiDeviceName));
        }
        return Ok(names);
    }
    anyhow::bail!("显示拓扑正在变化，请重试")
}
fn dimensions(monitor: &Monitor, width: Option<u32>) -> Result<(u32, u32)> {
    let width = match width {
        Some(w) => {
            ensure!(
                (640..=monitor.width).contains(&w),
                "采集宽度必须为 640–{} 像素",
                monitor.width
            );
            w
        }
        None => monitor.width,
    };
    let height =
        (f64::from(monitor.height) * f64::from(width) / f64::from(monitor.width)).round() as u32;
    ensure!(
        height > 0 && u64::from(width) * u64::from(height) <= 16_000_000,
        "采集超过 1600 万像素或尺寸无效"
    );
    Ok((width, height))
}

struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(null_mut(), self.0);
        }
    }
}
struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.0);
        }
    }
}
struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.0);
        }
    }
}
struct Selected(HDC, HGDIOBJ);
impl Drop for Selected {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.0, self.1);
        }
    }
}

fn jpeg(
    monitor: &Monitor,
    width: u32,
    height: u32,
    inspect: bool,
) -> Result<(Vec<u8>, Option<bool>)> {
    unsafe {
        let screen = ScreenDc(GetDC(null_mut()));
        ensure!(!screen.0.is_null(), "无法获取桌面 DC");
        let memory = MemoryDc(CreateCompatibleDC(screen.0));
        ensure!(!memory.0.is_null(), "无法创建采集 DC");
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = null_mut();
        let bitmap = Bitmap(CreateDIBSection(
            screen.0,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        ));
        ensure!(!bitmap.0.is_null() && !bits.is_null(), "无法分配画面缓冲区");
        let previous = SelectObject(memory.0, bitmap.0);
        ensure!(
            !previous.is_null() && previous as isize != -1,
            "无法选择采集位图"
        );
        let _selected = Selected(memory.0, previous);
        SetStretchBltMode(memory.0, HALFTONE);
        SetBrushOrgEx(memory.0, 0, 0, null_mut());
        checked(
            StretchBlt(
                memory.0,
                0,
                0,
                width as i32,
                height as i32,
                screen.0,
                monitor.x,
                monitor.y,
                monitor.width as i32,
                monitor.height as i32,
                SRCCOPY | CAPTUREBLT,
            ),
            "采集桌面画面",
        )?;
        let mut cursor = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if GetCursorInfo(&mut cursor) != 0 && cursor.flags == CURSOR_SHOWING {
            let mut icon = ICONINFO::default();
            if GetIconInfo(cursor.hCursor, &mut icon) != 0 {
                let _mask = Bitmap(icon.hbmMask);
                let _color = Bitmap(icon.hbmColor);
                let sx = f64::from(width) / f64::from(monitor.width);
                let sy = f64::from(height) / f64::from(monitor.height);
                DrawIconEx(
                    memory.0,
                    ((cursor.ptScreenPos.x - monitor.x - icon.xHotspot as i32) as f64 * sx).round()
                        as i32,
                    ((cursor.ptScreenPos.y - monitor.y - icon.yHotspot as i32) as f64 * sy).round()
                        as i32,
                    cursor.hCursor,
                    (f64::from(GetSystemMetrics(SM_CXCURSOR)) * sx)
                        .round()
                        .max(1.0) as i32,
                    (f64::from(GetSystemMetrics(SM_CYCURSOR)) * sy)
                        .round()
                        .max(1.0) as i32,
                    0,
                    null_mut(),
                    DI_NORMAL,
                );
            }
        }
        checked(GdiFlush(), "刷新采集缓冲区")?;
        let pixels =
            std::slice::from_raw_parts(bits.cast::<u8>(), width as usize * height as usize * 4);
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for pixel in pixels.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        // A generated frame does not prove the login UI was rendered. Record
        // only this boolean on startup/desktop transition, never pixels/colors.
        let uniform = inspect.then(|| {
            rgb.as_chunks::<3>()
                .0
                .iter()
                .all(|pixel| pixel == &rgb[..3])
        });
        let mut result = Vec::new();
        JpegEncoder::new_with_quality(&mut result, 85).encode(
            &rgb,
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )?;
        Ok((result, uniform))
    }
}

struct Capture {
    monitor: Monitor,
    displays: Vec<Monitor>,
    frames: watch::Receiver<Option<FrameEvent>>,
    size: watch::Sender<(u32, u32)>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Capture {
    fn start(monitor: Monitor, displays: Vec<Monitor>) -> Result<Self> {
        let (width, height) = dimensions(&monitor, None)?;
        crate::diagnostics::log(&format!(
            "GDI capture starting: {width}x{height}; {} monitors; selected={monitor:?}; displays={displays:?}",
            displays.len(),
        ));
        let (tx, frames) = watch::channel(None);
        let (size, sizes) = watch::channel((width, height));
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let selected = monitor.clone();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("landesk-gdi".into())
            .spawn(move || {
                let mut desktop = Desktop::new();
                let mut sequence = 0;
                let mut failing_since = None;
                let mut ready = Some(ready);
                while !cancelled.load(Ordering::Acquire) {
                    let began = Instant::now();
                    let result = (|| {
                        let changed = desktop.sync()?;
                        let (width, height) = *sizes.borrow();
                        jpeg(&selected, width, height, ready.is_some() || changed)
                    })();
                    match result {
                        Ok((jpeg, uniform)) => {
                            if let Some(uniform) = uniform {
                                crate::diagnostics::log(&format!(
                                    "GDI sampled frame: uniform={uniform}; desktop={}",
                                    super::desktop::context()
                                ));
                            }
                            failing_since = None;
                            sequence += 1;
                            tx.send_replace(Some(FrameEvent::Frame {
                                jpeg: Arc::new(jpeg),
                                sequence,
                            }));
                            if let Some(ready) = ready.take() {
                                crate::diagnostics::log("GDI first frame ready");
                                let _ = ready.send(Ok(()));
                            }
                        }
                        Err(error) => {
                            // Desktop transitions briefly reject GDI access. Retry the same
                            // desktop/monitor, never switch to another display or session.
                            let since = failing_since.get_or_insert_with(Instant::now);
                            if since.elapsed() >= Duration::from_secs(5) {
                                let message = format!("Windows 画面采集失败: {error:#}");
                                if let Some(ready) = ready.take() {
                                    let _ = ready.send(Err(message.clone()));
                                }
                                tx.send_replace(Some(FrameEvent::Error(message)));
                                break;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100).saturating_sub(began.elapsed()));
                }
            })?;
        let capture = Self {
            monitor,
            displays,
            frames,
            size,
            stop,
            worker: Some(worker),
        };
        started
            .recv_timeout(Duration::from_secs(10))
            .context("Windows 采集首帧超时")?
            .map_err(anyhow::Error::msg)?;
        Ok(capture)
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl FrameSource for Capture {
    fn frames(&self) -> &watch::Receiver<Option<FrameEvent>> {
        &self.frames
    }
    fn frames_mut(&mut self) -> &mut watch::Receiver<Option<FrameEvent>> {
        &mut self.frames
    }
    fn display_id(&self) -> u32 {
        self.monitor.id
    }
    fn native_width(&self) -> u32 {
        self.monitor.width
    }
    fn geometry(&self) -> [f64; 4] {
        monitor(self.monitor.id)
            .map(|m| [m.x as f64, m.y as f64, m.width as f64, m.height as f64])
            .unwrap_or([0.0; 4])
    }
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()> {
        self.size.send_replace(dimensions(&self.monitor, width)?);
        Ok(())
    }
    fn info(&self, requested: Option<u32>) -> CaptureInfo {
        let (width, height) = *self.size.borrow();
        CaptureInfo {
            width,
            height,
            native_width: self.monitor.width,
            native_height: self.monitor.height,
            requested_width: requested,
            display_id: self.monitor.id,
            displays: self
                .displays
                .iter()
                .map(|m| DisplayInfo {
                    id: m.id,
                    width: m.width,
                    height: m.height,
                    main: m.main,
                    virtual_display: m.virtual_display,
                })
                .collect(),
        }
    }
}

#[derive(Default)]
pub struct WindowsCapture {
    selected: Option<u32>,
}
impl CaptureBackend for WindowsCapture {
    fn open(&mut self, target: Option<DisplayTarget>) -> Result<Box<dyn FrameSource>> {
        let displays = monitors()?;
        let selected = match target {
            None => match self.selected {
                Some(id) => displays.iter().find(|m| m.id == id),
                None => displays.iter().find(|m| m.main),
            },
            Some(DisplayTarget::Existing { id }) => displays
                .iter()
                .find(|m| id.map(|id| id == m.id).unwrap_or(m.main)),
            Some(DisplayTarget::Virtual { width, height }) => {
                ensure!(
                    width == 1920 && height == 1080,
                    "内置 Windows 虚拟屏幕固定为 1920×1080；请使用采集分辨率调整传输大小"
                );
                displays.iter().find(|m| m.virtual_display)
            }
        }
        .context("指定屏幕未启用，请检查显示驱动和 Windows 显示设置")?
        .clone();
        let capture = Capture::start(selected.clone(), displays)?;
        self.selected = Some(selected.id);
        Ok(Box::new(capture))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimensions_bound_memory_and_preserve_aspect() {
        let m = Monitor {
            id: 1,
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
            main: false,
            virtual_display: true,
        };
        assert_eq!(dimensions(&m, Some(1280)).unwrap(), (1280, 720));
        assert!(dimensions(&m, Some(639)).is_err());
        assert!(dimensions(&m, Some(1921)).is_err());
        assert!(
            dimensions(
                &Monitor {
                    width: 8000,
                    height: 8000,
                    ..m
                },
                None
            )
            .is_err()
        );
    }
}
