use anyhow::{Result, ensure};
use core_foundation::{
    base::TCFType,
    string::{CFString, CFStringRef},
};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        kind: CFStringRef,
        level: u32,
        name: CFStringRef,
        id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// Keeps the authenticated session running while allowing the display to sleep.
/// Does not override lid-close, explicit sleep, or low-battery sleep.
pub struct KeepAwake(u32);

impl KeepAwake {
    pub fn new() -> Result<Self> {
        let kind = CFString::new("PreventUserIdleSystemSleep");
        let name = CFString::new("LanDesk remote session");
        let mut id = 0;
        let result = unsafe {
            IOPMAssertionCreateWithName(
                kind.as_concrete_TypeRef(),
                255,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        ensure!(result == 0, "无法保持远控期间系统运行：IOKit {result}");
        Ok(Self(id))
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        let result = unsafe { IOPMAssertionRelease(self.0) };
        if result != 0 {
            eprintln!("释放 LanDesk 电源断言失败：IOKit {result}");
        }
    }
}
