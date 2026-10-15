use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU32;
#[derive(Default)]
pub struct Shared {
    pub active: AtomicBool,
    pub shutdown: AtomicBool,
    pub display: Arc<AtomicU32>,
}
