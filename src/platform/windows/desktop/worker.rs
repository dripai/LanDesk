use super::{
    dxgi, sys,
    wire::{self, Action, Event, Request},
};
use anyhow::{Result, ensure};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub fn dimensions(w: u32, h: u32, requested: u32) -> Result<(u32, u32)> {
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
pub fn run() -> Result<()> {
    ensure!(
        sys::current_is_system()?,
        "桌面进程必须由 LanDesk 后台服务启动"
    );
    let name = std::env::args()
        .nth(2)
        .ok_or_else(|| anyhow::anyhow!("缺少桌面启动通道"))?;
    let pipe = super::bootstrap::Mapping::receive(&name)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel(64);
    let output = Arc::new(Mutex::new(super::worker_io::start(
        pipe,
        tx,
        stopped.clone(),
    )?));
    let requests = Arc::new(Mutex::new(rx));
    let mut requested = 0;
    let mut original = None;
    let mut transition_started = None;
    while !stopped.load(Ordering::Acquire) {
        let requests = requests.clone();
        let stopped = stopped.clone();
        let output_clone = output.clone();
        // Each desktop owns a fresh OS thread: no windows or hooks can pin the old desktop.
        let (result, next, initial, produced_frame) = std::thread::spawn(move || {
            let mut requested = requested;
            let mut initial = original;
            let mut produced_frame = false;
            let result = desktop_loop(
                &requests,
                &stopped,
                &mut requested,
                &mut initial,
                &output_clone,
                &mut produced_frame,
            );
            (result, requested, initial, produced_frame)
        })
        .join()
        .map_err(|_| anyhow::anyhow!("桌面线程异常退出"))?;
        requested = next;
        original = initial;
        if produced_frame {
            transition_started = None;
        }
        if let Err(error) = result {
            let recoverable = transient(&error);
            let since = transition_started.get_or_insert_with(std::time::Instant::now);
            if !recoverable || since.elapsed() > Duration::from_secs(10) {
                wire::event(
                    &mut *output.lock().unwrap(),
                    Event::Error {
                        message: format!("{error:#}"),
                    },
                )?;
                return Err(error);
            }
            std::thread::sleep(Duration::from_millis(100));
        } else {
            transition_started = None;
        }
    }
    Ok(())
}
fn desktop_loop(
    requests: &Mutex<mpsc::Receiver<Request>>,
    stopped: &AtomicBool,
    requested: &mut u32,
    original: &mut Option<(u32, u32)>,
    output: &Mutex<super::worker_io::Output>,
    produced_frame: &mut bool,
) -> Result<()> {
    let mut desktop = sys::Desktop::open()?;
    desktop.bind()?;
    let capture = dxgi::Capture::new()?;
    let size = (capture.width, capture.height);
    let mut output = output.lock().unwrap();
    if let Some(previous) = *original {
        ensure!(previous == size, "显示器尺寸变化，请重新连接");
    } else {
        wire::event(
            &mut *output,
            Event::Ready {
                version: wire::VERSION,
                width: size.0,
                height: size.1,
            },
        )?;
        *original = Some(size);
    }
    let mut input = crate::platform::enigo_input::Input::new(size.0 as i32, size.1 as i32)?;
    let result = (|| -> Result<()> {
        let mut last_frame = None;
        while !stopped.load(Ordering::Acquire) {
            let mut dirty = false;
            if sys::Desktop::open()?.name != desktop.name {
                return Ok(());
            }
            for _ in 0..128 {
                let request = match requests.lock().unwrap().try_recv() {
                    Ok(value) => value,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(_) => return Ok(()),
                };
                let result = match request.action {
                    Action::Input { message } => input.handle(message),
                    Action::Paste => input.paste(),
                    Action::Release => input.release_all(),
                    Action::Resolution { width } => dimensions(size.0, size.1, width).map(|_| {
                        *requested = width;
                        dirty = true;
                    }),
                };
                wire::event(
                    &mut *output,
                    Event::Reply {
                        id: request.id,
                        error: result.err().map(|e| format!("{e:#}")),
                    },
                )?;
            }
            if let Some(rgb) = capture.frame()? {
                last_frame = Some(rgb);
                dirty = true;
            }
            if let Some(rgb) = last_frame.as_ref().filter(|_| dirty) {
                let (w, h) = dimensions(size.0, size.1, *requested)?;
                let rgb = if (w, h) == size {
                    rgb.clone()
                } else {
                    image::imageops::resize(rgb, w, h, FilterType::Triangle)
                };
                let mut bytes = Vec::new();
                JpegEncoder::new_with_quality(&mut bytes, 92).encode_image(&rgb)?;
                wire::write_packet(&mut *output, 1, &bytes)?;
                *produced_frame = true;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        Ok(())
    })();
    // Release held keys on the currently active desktop, including a just-opened
    // lock/UAC desktop; SendInput on the old desktop may no longer be accepted.
    let cleanup = (|| -> Result<()> {
        let mut current = sys::Desktop::open()?;
        current.bind()?;
        input.release_all()
    })();
    result.and(cleanup)
}

fn transient(error: &anyhow::Error) -> bool {
    use windows::Win32::Graphics::Dxgi::*;
    if let Some(error) = error.downcast_ref::<windows::core::Error>() {
        return matches!(
            error.code(),
            windows::Win32::Foundation::E_ACCESSDENIED
                | DXGI_ERROR_UNSUPPORTED
                | DXGI_ERROR_ACCESS_LOST
                | DXGI_ERROR_SESSION_DISCONNECTED
                | DXGI_ERROR_DEVICE_REMOVED
                | DXGI_ERROR_DEVICE_RESET
                | DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
        );
    }
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| matches!(error.raw_os_error(), Some(5 | 170)))
}
