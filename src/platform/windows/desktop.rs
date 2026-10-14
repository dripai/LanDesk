use anyhow::{Context, Result, ensure};
use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
};

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub fn checked(ok: i32, operation: &str) -> Result<()> {
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context(operation.to_owned());
    }
    Ok(())
}
pub struct Handle(pub HANDLE);
impl Handle {
    pub fn new(raw: HANDLE, operation: &str) -> Result<Self> {
        if raw.is_null() {
            return Err(std::io::Error::last_os_error()).context(operation.to_owned());
        }
        Ok(Self(raw))
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// No windows or hooks are created on these threads. Every GDI object is released
// before changing desktops, including transitions to Winlogon/UAC.
pub struct Desktop {
    original: HDESK,
    current: HDESK,
    name: String,
}
impl Desktop {
    pub fn new() -> Self {
        Self {
            original: unsafe { GetThreadDesktop(GetCurrentThreadId()) },
            current: null_mut(),
            name: String::new(),
        }
    }
    pub fn sync(&mut self) -> Result<bool> {
        unsafe {
            let next = OpenInputDesktop(
                0,
                0,
                DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS | DESKTOP_SWITCHDESKTOP,
            );
            ensure!(
                !next.is_null(),
                "打开输入桌面失败: {}",
                std::io::Error::last_os_error()
            );
            let result = (|| {
                let mut name = [0u16; 256];
                let mut needed = 0;
                checked(
                    GetUserObjectInformationW(
                        next,
                        UOI_NAME,
                        name.as_mut_ptr().cast(),
                        size_of::<[u16; 256]>() as u32,
                        &mut needed,
                    ),
                    "读取桌面名称",
                )?;
                let name = String::from_utf16_lossy(
                    &name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())],
                );
                checked(SetThreadDesktop(next), "切换采集/输入线程桌面")?;
                let changed = !self.name.is_empty() && self.name != name;
                self.name = name;
                Ok(changed)
            })();
            if result.is_ok() {
                if !self.current.is_null() {
                    CloseDesktop(self.current);
                }
                self.current = next;
            } else {
                CloseDesktop(next);
            }
            result
        }
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        unsafe {
            SetThreadDesktop(self.original);
            if !self.current.is_null() {
                CloseDesktop(self.current);
            }
        }
    }
}
