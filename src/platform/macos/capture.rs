use super::virtual_display::MacVirtualDisplayProvider;
use crate::capture::FrameEvent;
use crate::platform::{
    CaptureBackend, DisplayTarget, FrameSource, VirtualDisplay, VirtualDisplaySpec, VirtualDisplays,
};
use anyhow::{Context, Result, ensure};
use image::codecs::jpeg::JpegEncoder;
use screencapturekit::{cm::CMSampleBufferExt, cv::CVPixelBufferLockFlags, prelude::*};
use std::rc::Rc;
use std::{
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> screencapturekit::cg::CGRect;
}

pub fn display_bounds(display: u32) -> screencapturekit::cg::CGRect {
    unsafe { CGDisplayBounds(display) }
}

const JPEG_QUALITY: u8 = 92;
const MAX_RGB_BYTES: usize = 48_000_000;

fn capture_dimensions(point_width: f64, point_height: f64, scale: f32) -> Result<(u32, u32)> {
    ensure!(
        point_width.is_finite()
            && point_height.is_finite()
            && scale.is_finite()
            && point_width > 0.0
            && point_height > 0.0
            && scale > 0.0,
        "显示器像素尺寸无效"
    );
    let width = (point_width * f64::from(scale)).round();
    let height = (point_height * f64::from(scale)).round();
    ensure!(
        width >= 1.0
            && height >= 1.0
            && width <= f64::from(u32::MAX)
            && height <= f64::from(u32::MAX),
        "显示器像素尺寸无效"
    );
    ensure!(
        width * height * 3.0 <= MAX_RGB_BYTES as f64,
        "显示器像素尺寸超过 1600 万像素上限"
    );
    Ok((width as u32, height as u32))
}

pub struct Capture {
    stream: SCStream,
    _virtual_display: Option<Rc<dyn VirtualDisplay>>,
    pub frames: watch::Receiver<Option<FrameEvent>>,
    pub width: u32,
    pub height: u32,
    native_width: u32,
    native_height: u32,
    pub display_id: u32,
    pub displays: Vec<serde_json::Value>,
}

fn resolution_dimensions(
    native_width: u32,
    native_height: u32,
    width: Option<u32>,
) -> Result<(u32, u32)> {
    let Some(width) = width else {
        return Ok((native_width, native_height));
    };
    ensure!(
        (640..=native_width).contains(&width),
        "采集宽度必须为 640–{native_width} 像素"
    );
    let height =
        (f64::from(native_height) * f64::from(width) / f64::from(native_width)).round() as u32;
    ensure!(height > 0, "采集高度无效");
    Ok((width, height))
}

fn configuration(width: u32, height: u32) -> SCStreamConfiguration {
    SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        .with_shows_cursor(true)
        .with_minimum_frame_interval(&CMTime::new(1, 15))
}

struct Handler {
    tx: watch::Sender<Option<FrameEvent>>,
    sequence: AtomicU64,
}

fn bgra_to_rgb(data: &[u8], width: usize, height: usize, stride: usize) -> Result<Vec<u8>> {
    let row = width.checked_mul(4).context("画面宽度溢出")?;
    ensure!(width > 0 && height > 0 && stride >= row, "像素行布局无效");
    let needed = stride
        .checked_mul(height - 1)
        .and_then(|n| n.checked_add(row))
        .context("画面尺寸溢出")?;
    ensure!(data.len() >= needed, "画面缓冲区长度不足");
    let length = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .context("画面尺寸溢出")?;
    ensure!(length <= MAX_RGB_BYTES, "画面尺寸超过 1600 万像素上限");
    let mut rgb = Vec::with_capacity(length);
    for y in 0..height {
        for pixel in data[y * stride..y * stride + row].chunks_exact(4) {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    Ok(rgb)
}

impl SCStreamOutputTrait for Handler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, kind: SCStreamOutputType) {
        if !matches!(kind, SCStreamOutputType::Screen) {
            return;
        }
        // Idle / blank transition samples may have no image; they are not capture errors.
        let Some(buffer) = sample.pixel_buffer() else {
            return;
        };
        let result = (|| -> Result<Vec<u8>> {
            let lock = buffer
                .lock(CVPixelBufferLockFlags::READ_ONLY)
                .map_err(|e| anyhow::anyhow!("无法读取画面: {e:?}"))?;
            // The guard holds the pixel-buffer lock until after conversion.
            let data = unsafe { lock.as_slice() }.context("画面缓冲区不可读")?;
            let rgb = bgra_to_rgb(
                data,
                buffer.width(),
                buffer.height(),
                buffer.bytes_per_row(),
            )?;
            let mut jpeg = Vec::new();
            JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY).encode(
                &rgb,
                buffer.width() as u32,
                buffer.height() as u32,
                image::ExtendedColorType::Rgb8,
            )?;
            Ok(jpeg)
        })();
        let event = match result {
            Ok(jpeg) => FrameEvent::Frame {
                jpeg: Arc::new(jpeg),
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            },
            Err(e) => FrameEvent::Error(e.to_string()),
        };
        self.tx.send_if_modified(|current| {
            if matches!(current, Some(FrameEvent::Error(_))) {
                return false;
            }
            *current = Some(event);
            true
        });
    }
}

impl Capture {
    fn start(display_id: Option<u32>) -> Result<Self> {
        let content = SCShareableContent::get()
            .context("无法采集屏幕，请授予 LanDesk 屏幕录制权限并重启应用")?;
        let displays = content.displays();
        let display_id = display_id.unwrap_or_else(|| unsafe { CGMainDisplayID() });
        let display = displays
            .iter()
            .find(|display| display.display_id() == display_id)
            .context("选定显示器已不可用，请重新连接或选择其他显示器")?;
        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .with_include_menu_bar(true)
            .build()?;
        // ScreenCaptureKit reports content in points; preserve Retina backing pixels.
        let content_rect = filter.content_rect();
        let (width, height) = capture_dimensions(
            content_rect.size.width,
            content_rect.size.height,
            filter.point_pixel_scale(),
        )?;
        let config = configuration(width, height);
        let (tx, frames) = watch::channel(None);
        let errors = tx.clone();
        let delegate = screencapturekit::stream::ErrorHandler::new(move |error| {
            eprintln!("LanDesk capture stopped: {error}");
            errors.send_replace(Some(FrameEvent::Error(error.to_string())));
        });
        let mut stream = SCStream::new_with_delegate(&filter, &config, delegate)?;
        stream.add_output_handler(
            Handler {
                tx,
                sequence: AtomicU64::new(0),
            },
            SCStreamOutputType::Screen,
        )?;
        stream.start_capture()?;
        Ok(Self {
            stream,
            _virtual_display: None,
            frames,
            width,
            height,
            native_width: width,
            native_height: height,
            display_id,
            displays: displays.iter().map(|display| serde_json::json!({
                "id":display.display_id(), "width":display.width(), "height":display.height(),
                "main":display.display_id() == unsafe { CGMainDisplayID() },
            })).collect(),
        })
    }
    pub fn set_resolution(&mut self, requested_width: Option<u32>) -> Result<()> {
        let (width, height) =
            resolution_dimensions(self.native_width, self.native_height, requested_width)?;
        self.stream
            .update_configuration(&configuration(width, height))
            .context("更新采集分辨率失败")?;
        self.width = width;
        self.height = height;
        Ok(())
    }
}

impl Capture {
    fn info(&self, requested: Option<u32>) -> serde_json::Value {
        serde_json::json!({"type":"display_state", "width":self.width,"height":self.height,
            "native_width":self.native_width,"native_height":self.native_height,
            "requested_width":requested,"display_id":self.display_id,"displays":self.displays})
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Err(e) = self.stream.stop_capture() {
            eprintln!("停止画面采集失败: {e}");
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
    fn info(&self, requested: Option<u32>) -> serde_json::Value {
        Capture::info(self, requested)
    }
    fn display_id(&self) -> u32 {
        self.display_id
    }
    fn native_width(&self) -> u32 {
        self.native_width
    }
    fn geometry(&self) -> [f64; 4] {
        let r = display_bounds(self.display_id);
        [r.origin.x, r.origin.y, r.size.width, r.size.height]
    }
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()> {
        Capture::set_resolution(self, width)
    }
}

pub struct MacCaptureBackend {
    virtuals: VirtualDisplays,
    selected: Option<DisplayTarget>,
}
impl Default for MacCaptureBackend {
    fn default() -> Self {
        Self {
            virtuals: VirtualDisplays::new(MacVirtualDisplayProvider),
            selected: None,
        }
    }
}
impl CaptureBackend for MacCaptureBackend {
    fn open(&mut self, target: Option<DisplayTarget>) -> Result<Box<dyn FrameSource>> {
        let target = target
            .or(self.selected)
            .unwrap_or(DisplayTarget::Existing { id: None });
        let (id, lease, spec) = match target {
            DisplayTarget::Existing { id } => (id, id.and_then(|id| self.virtuals.find(id)), None),
            DisplayTarget::Virtual { width, height } => {
                let spec = VirtualDisplaySpec { width, height }.validate()?;
                let display = self.virtuals.prepare(spec)?;
                (Some(display.id()), Some(display), Some(spec))
            }
        };
        let mut capture = Capture::start(id)?;
        if spec.is_some() {
            if let Some(previous) = self.virtuals.current_id() {
                if previous != capture.display_id {
                    // The old stream still owns this display until the caller
                    // swaps streams; omit the device that is about to disappear.
                    capture.displays.retain(|display| display["id"] != previous);
                }
            }
        }
        for display in &mut capture.displays {
            let id = display["id"].as_u64().unwrap_or(0) as u32;
            display["virtual"] = (self.virtuals.find(id).is_some()
                || lease.as_ref().is_some_and(|d| d.id() == id))
            .into();
        }
        capture._virtual_display = lease.clone();
        if let (Some(spec), Some(display)) = (spec, lease) {
            self.virtuals.commit(spec, display);
        }
        self.selected = Some(target);
        Ok(Box::new(capture))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_resolution_preserves_aspect_and_original_mode() {
        assert_eq!(
            resolution_dimensions(2880, 1800, None).unwrap(),
            (2880, 1800)
        );
        assert_eq!(
            resolution_dimensions(2880, 1800, Some(1920)).unwrap(),
            (1920, 1200)
        );
        assert_eq!(
            resolution_dimensions(1920, 1080, Some(1280)).unwrap(),
            (1280, 720)
        );
        assert!(resolution_dimensions(2880, 1800, Some(639)).is_err());
        assert!(resolution_dimensions(2880, 1800, Some(3000)).is_err());
    }
    #[test]
    fn capture_preserves_retina_pixels_and_display_aspect_ratio() {
        assert_eq!(
            capture_dimensions(1440.0, 900.0, 2.0).unwrap(),
            (2880, 1800)
        );
        assert_eq!(
            capture_dimensions(1920.0, 1080.0, 1.0).unwrap(),
            (1920, 1080)
        );
        assert_eq!(
            capture_dimensions(2560.0, 1440.0, 2.0).unwrap(),
            (5120, 2880)
        );
    }

    #[test]
    fn capture_rejects_invalid_or_oversized_dimensions() {
        for (width, height, scale) in [
            (0.0, 900.0, 2.0),
            (-1440.0, 900.0, 2.0),
            (1440.0, 900.0, 0.0),
            (f64::NAN, 900.0, 2.0),
            (1440.0, 900.0, f32::INFINITY),
            (7680.0, 4320.0, 1.0),
        ] {
            assert!(capture_dimensions(width, height, scale).is_err());
        }
    }

    #[test]
    fn row_padding_is_not_encoded_as_pixels() {
        let data = [1, 2, 3, 255, 9, 9, 9, 9, 4, 5, 6, 255];
        assert_eq!(bgra_to_rgb(&data, 1, 2, 8).unwrap(), [3, 2, 1, 6, 5, 4]);
        assert!(bgra_to_rgb(&data[..10], 1, 2, 8).is_err());
        assert!(bgra_to_rgb(&data, 1, 2, 3).is_err());
    }
}
