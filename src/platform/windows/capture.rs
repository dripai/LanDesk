use super::desktop::{broker::Broker, wire::Action, worker::dimensions};
use crate::platform::{CaptureSession, FrameEvent};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::watch;
pub struct Capture {
    broker: Arc<Broker>,
    width: u32,
    height: u32,
    requested: u32,
    frames: watch::Receiver<Option<FrameEvent>>,
}
impl Capture {
    pub fn start() -> Result<Self> {
        let (broker, width, height, frames) = Broker::start()?;
        Ok(Self {
            broker,
            width,
            height,
            requested: 0,
            frames,
        })
    }
}
impl CaptureSession for Capture {
    fn dimensions(&self) -> (u32, u32) {
        dimensions(self.width, self.height, self.requested).expect("validated dimensions")
    }
    fn input_dimensions(&self) -> (i32, i32) {
        (self.width as i32, self.height as i32)
    }
    fn frames(&self) -> watch::Receiver<Option<FrameEvent>> {
        self.frames.clone()
    }
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()> {
        let width = width.unwrap_or(0);
        dimensions(self.width, self.height, width)?;
        self.broker.call(Action::Resolution { width })?;
        self.requested = width;
        Ok(())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.broker.close();
    }
}
