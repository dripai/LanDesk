//! Windows console desktop service and persistent display component.
mod bootstrap;
mod capture;
mod desktop;
mod input;
mod interactive;
mod pipe;
mod service;
mod setup;
mod setup_dialog;
mod setup_services;
mod setup_support;
mod ui;
pub mod virtual_display;
pub use service::run;
