//! Windows console desktop service and persistent display component.
mod capture;
mod desktop;
mod input;
mod service;
pub mod virtual_display;
pub use service::run;
