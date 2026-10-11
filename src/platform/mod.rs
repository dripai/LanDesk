//! OS-independent contracts. Native objects stay on their platform-owned threads.
#[cfg(target_os = "macos")]
pub mod macos;

use crate::protocol::ClientMessage;
use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32},
};

#[derive(Default)]
pub struct Shared {
    pub active: AtomicBool,
    pub shutdown: AtomicBool,
    pub display: Arc<AtomicU32>,
}

#[derive(Serialize)]
pub struct Capabilities {
    pub os: &'static str,
    /// A provider is implemented; creation still needs to succeed on the device.
    pub virtual_display: bool,
}

pub trait DesktopControl: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn check_permissions(&self) -> Result<()>;
    fn begin_input(&self, display_id: u32) -> BoxFuture<'_, Result<()>>;
    fn input(&self, message: ClientMessage) -> BoxFuture<'_, Result<()>>;
    fn paste_text(&self, text: String) -> BoxFuture<'_, Result<()>>;
    fn paste_image(&self, png: Vec<u8>) -> BoxFuture<'_, Result<()>>;
    fn copy_text(&self) -> BoxFuture<'_, Result<String>>;
    fn read_clipboard(&self) -> BoxFuture<'_, Result<String>>;
    fn end_session(&self) -> BoxFuture<'_, Result<()>>;
    fn restore(&self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualDisplaySpec {
    pub width: u32,
    pub height: u32,
}
impl VirtualDisplaySpec {
    pub fn validate(self) -> Result<Self> {
        anyhow::ensure!(self.width > 0 && self.height > 0, "虚拟屏幕尺寸必须大于零");
        // Same memory budget as the existing JPEG capture path.
        anyhow::ensure!(
            u64::from(self.width) * u64::from(self.height) <= 16_000_000,
            "虚拟屏幕超过当前采集支持的 1600 万像素"
        );
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayTarget {
    Existing { id: Option<u32> },
    Virtual { width: u32, height: u32 },
}

/// Owned on the capture worker, never sent across AppKit/OS thread boundaries.
pub trait FrameSource {
    fn frames(&self) -> &tokio::sync::watch::Receiver<Option<crate::capture::FrameEvent>>;
    fn frames_mut(
        &mut self,
    ) -> &mut tokio::sync::watch::Receiver<Option<crate::capture::FrameEvent>>;
    fn info(&self, requested_width: Option<u32>) -> serde_json::Value;
    fn display_id(&self) -> u32;
    fn native_width(&self) -> u32;
    fn geometry(&self) -> [f64; 4];
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()>;
}

pub trait CaptureBackend {
    /// None reconnects to the last successfully selected source.
    /// Errors leave the previous selection and its display lease intact.
    fn open(&mut self, target: Option<DisplayTarget>) -> Result<Box<dyn FrameSource>>;
}

pub trait VirtualDisplay {
    fn id(&self) -> u32;
}
pub trait VirtualDisplayProvider {
    fn create(&self, spec: VirtualDisplaySpec) -> Result<std::rc::Rc<dyn VirtualDisplay>>;
}

/// Preparing is transactional: retain the old display until capture of the new
/// one has succeeded. Committing keeps it alive across browser disconnects.
pub struct VirtualDisplays {
    provider: Box<dyn VirtualDisplayProvider>,
    current: Option<(VirtualDisplaySpec, std::rc::Rc<dyn VirtualDisplay>)>,
}
impl VirtualDisplays {
    pub fn new(provider: impl VirtualDisplayProvider + 'static) -> Self {
        Self {
            provider: Box::new(provider),
            current: None,
        }
    }
    pub fn prepare(&self, spec: VirtualDisplaySpec) -> Result<std::rc::Rc<dyn VirtualDisplay>> {
        let spec = spec.validate()?;
        if let Some((current, display)) = &self.current {
            if *current == spec {
                return Ok(display.clone());
            }
        }
        self.provider.create(spec)
    }
    pub fn commit(&mut self, spec: VirtualDisplaySpec, display: std::rc::Rc<dyn VirtualDisplay>) {
        self.current = Some((spec, display));
    }
    pub fn find(&self, id: u32) -> Option<std::rc::Rc<dyn VirtualDisplay>> {
        self.current
            .as_ref()
            .filter(|(_, d)| d.id() == id)
            .map(|(_, d)| d.clone())
    }
    pub fn current_id(&self) -> Option<u32> {
        self.current.as_ref().map(|(_, display)| display.id())
    }
}

pub trait FileSession: Send {
    fn message(&mut self, message: ClientMessage) -> serde_json::Value;
    fn chunk(&mut self, bytes: &[u8]) -> serde_json::Value;
}
pub trait FileBackend: Send + Sync {
    fn open_session(self: Arc<Self>, cancelled: Arc<AtomicBool>) -> Box<dyn FileSession>;
}
