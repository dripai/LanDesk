use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
mod commands;
pub use commands::*;
pub mod password;
#[derive(Serialize)]
pub struct Capabilities {
    pub os: &'static str,
    /// A provider is implemented; creation still needs to succeed on the device.
    pub virtual_display: bool,
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

pub mod connection;
pub mod identity;

#[derive(Clone)]
pub enum FrameEvent {
    Frame {
        jpeg: std::sync::Arc<Vec<u8>>,
        sequence: u64,
    },
    Error(String),
}
