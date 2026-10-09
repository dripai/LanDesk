use crate::platform::{CaptureSession, FrameEvent};
use anyhow::{Context, Result, ensure};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::watch;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
};
use xcap::Monitor;

static CAPTURING: AtomicBool = AtomicBool::new(false);
struct CaptureSlot;
impl Drop for CaptureSlot {
    fn drop(&mut self) {
        CAPTURING.store(false, Ordering::Release);
    }
}

pub fn check_desktop() -> Result<()> {
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        ensure!(
            !desktop.is_null(),
            "Windows 桌面不可访问，请在本机解锁并登录"
        );
        let mut name = [0u16; 256];
        let mut needed = 0;
        let ok = GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            (name.len() * 2) as u32,
            &mut needed,
        );
        CloseDesktop(desktop);
        ensure!(ok != 0, "无法检查当前 Windows 桌面");
        let end = name.iter().position(|x| *x == 0).unwrap_or(name.len());
        ensure!(
            String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("default"),
            "当前为锁屏或安全桌面，请在本机恢复普通桌面后重连"
        );
    }
    Ok(())
}

pub struct Capture {
    width: u32,
    height: u32,
    requested: Arc<AtomicU32>,
    stopped: Arc<AtomicBool>,
    frames: watch::Receiver<Option<FrameEvent>>,
}
fn dimensions(w: u32, h: u32, requested: u32) -> Result<(u32, u32)> {
    ensure!(
        w > 0 && h > 0 && u64::from(w) * u64::from(h) <= 16_000_000,
        "显示器尺寸超过限制"
    );
    if requested == 0 {
        return Ok((w, h));
    }
    ensure!(
        (640..=w).contains(&requested),
        "采集宽度必须为 640–{w} 像素"
    );
    Ok((
        requested,
        (f64::from(h) * f64::from(requested) / f64::from(w)).round() as u32,
    ))
}
impl Capture {
    pub fn start() -> Result<Self> {
        CAPTURING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("上次画面采集仍在停止，请稍后重连"))?;
        let slot = CaptureSlot;
        let (tx, frames) = watch::channel(None);
        let (ready, receive) = mpsc::sync_channel(1);
        let requested = Arc::new(AtomicU32::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let request = requested.clone();
        let stop = stopped.clone();
        std::thread::Builder::new()
            .name("landesk-capture".into())
            .spawn(move || {
                let _slot = slot;
                let result = (|| -> Result<()> {
                    check_desktop()?;
                    let monitors = Monitor::all()?;
                    ensure!(monitors.len() == 1, "当前版本仅支持一个显示器");
                    let monitor = &monitors[0];
                    let (w, h) = (monitor.width()?, monitor.height()?);
                    ensure!(monitor.x()? == 0 && monitor.y()? == 0, "显示器原点不受支持");
                    dimensions(w, h, 0)?;
                    ready.send(Ok((w, h)))?;
                    let mut sequence = 0;
                    while !stop.load(Ordering::Acquire) && !tx.is_closed() {
                        check_desktop()?;
                        ensure!(
                            Monitor::all()?.len() == 1
                                && monitor.width()? == w
                                && monitor.height()? == h,
                            "显示器发生变化，请重新连接"
                        );
                        let rgba = monitor.capture_image()?;
                        ensure!(rgba.dimensions() == (w, h), "采集尺寸已变化，请重新连接");
                        let (width, height) = dimensions(w, h, request.load(Ordering::Acquire))?;
                        let rgb = image::DynamicImage::ImageRgba8(rgba).into_rgb8();
                        let rgb = if (width, height) == (w, h) {
                            rgb
                        } else {
                            image::imageops::resize(&rgb, width, height, FilterType::Triangle)
                        };
                        let mut jpeg = Vec::new();
                        JpegEncoder::new_with_quality(&mut jpeg, 92).encode_image(&rgb)?;
                        sequence += 1;
                        if tx
                            .send(Some(FrameEvent::Frame {
                                jpeg: Arc::new(jpeg),
                                sequence,
                            }))
                            .is_err()
                        {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(66));
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    let message = format!("{error:#}");
                    let _ = ready.try_send(Err(anyhow::anyhow!(message.clone())));
                    let _ = tx.send(Some(FrameEvent::Error(message)));
                }
            })?;
        match receive.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok((width, height))) => Ok(Self {
                width,
                height,
                requested,
                stopped,
                frames,
            }),
            result => {
                stopped.store(true, Ordering::Release);
                match result {
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(error).context("启动 Windows 采集超时"),
                    _ => unreachable!(),
                }
            }
        }
    }
}
impl CaptureSession for Capture {
    fn dimensions(&self) -> (u32, u32) {
        dimensions(
            self.width,
            self.height,
            self.requested.load(Ordering::Acquire),
        )
        .expect("validated capture dimensions")
    }
    fn input_dimensions(&self) -> (i32, i32) {
        (self.width as i32, self.height as i32)
    }
    fn frames(&self) -> watch::Receiver<Option<FrameEvent>> {
        self.frames.clone()
    }
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()> {
        dimensions(self.width, self.height, width.unwrap_or(0))?;
        self.requested.store(width.unwrap_or(0), Ordering::Release);
        Ok(())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resizing_preserves_aspect_and_native_pixels() {
        assert_eq!(dimensions(2560, 1440, 0).unwrap(), (2560, 1440));
        assert_eq!(dimensions(2560, 1440, 1280).unwrap(), (1280, 720));
        assert!(dimensions(2560, 1440, 4000).is_err());
    }
}
