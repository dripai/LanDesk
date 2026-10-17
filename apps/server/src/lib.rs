pub mod adapters;
pub mod bootstrap;
pub mod clipboard_image;
pub mod diagnostics;
pub mod platform;
#[cfg(any(windows, test))]
mod setup;

#[cfg(any(windows, target_os = "macos"))]
mod settings_view;
