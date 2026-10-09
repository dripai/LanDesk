mod enigo_input;
use crate::{
    desktop::{Command, DesktopControl},
    protocol::ClientMessage,
};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc},
};
use tokio::sync::watch;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "macos")]
pub use macos::{MacPlatform as CurrentPlatform, files as filesystem, ssh};
#[cfg(target_os = "windows")]
pub use windows::{WindowsPlatform as CurrentPlatform, files as filesystem, ssh};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct Capabilities {
    pub capture: bool,
    pub input: bool,
    pub clipboard_text: bool,
    pub clipboard_image: bool,
    pub files: bool,
    pub capture_resize: bool,
    pub display_sleep: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerInfo {
    pub protocol_version: u32,
    pub os: &'static str,
    pub capabilities: Capabilities,
}

#[derive(Clone)]
pub enum FrameEvent {
    Frame { jpeg: Arc<Vec<u8>>, sequence: u64 },
    Error(String),
}

pub trait CaptureSession: Send {
    fn dimensions(&self) -> (u32, u32);
    fn input_dimensions(&self) -> (i32, i32);
    fn frames(&self) -> watch::Receiver<Option<FrameEvent>>;
    fn set_resolution(&mut self, width: Option<u32>) -> Result<()>;
}

pub trait InputController {
    fn handle(&mut self, message: ClientMessage) -> Result<()>;
    fn paste(&mut self) -> Result<()>;
    fn release_all(&mut self) -> Result<()>;
}

pub trait SessionPower {}

/// All native UI, input, clipboard and power methods run on the UI thread.
/// Capture and file factories may be called from worker threads.
pub trait HostPlatform: Send + Sync {
    fn info(&self) -> ServerInfo;
    fn check_permissions(&self) -> Result<()>;
    fn capture(&self) -> Result<Box<dyn CaptureSession>>;
    fn home_directory(&self) -> Result<PathBuf>;
    fn input(&self, width: i32, height: i32) -> Result<Box<dyn InputController>>;
    fn keep_awake(&self) -> Result<Box<dyn SessionPower>>;
    fn read_clipboard(&self) -> Result<String>;
    fn write_clipboard_image(&self, png: &[u8]) -> Result<()>;
    fn run_ui(&self, control: DesktopControl, commands: mpsc::Receiver<Command>) -> Result<()>;
}

/// Implementations must anchor access to the user's directory, reject links
/// that escape it, and publish completed uploads atomically without overwrite.
pub trait FileSystem: Send + Sync {
    fn list(&self, id: u32, path: &str) -> Result<Value>;
    fn start_upload(
        &self,
        id: u32,
        path: &str,
        name: &str,
        size: u64,
    ) -> Result<Box<dyn FileTransfer>>;
}

pub trait FileTransfer: Send {
    fn id(&self) -> u32;
    fn chunk(&mut self, data: &[u8]) -> Result<Value>;
    fn finish(&mut self, cancelled: &AtomicBool) -> Result<Value>;
    fn cleanup(&mut self) -> Result<()>;
}
