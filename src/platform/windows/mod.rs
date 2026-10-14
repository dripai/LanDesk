//! Windows console desktop service and persistent display component.
mod capture;
mod desktop;
mod input;
mod pipe;
mod service;
mod ui;
pub mod virtual_display;
pub use service::run;
