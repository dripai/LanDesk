use anyhow::{Context, Result, ensure};
use std::{mem::size_of, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
    System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
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
fn object_name(handle: HANDLE) -> Result<String> {
    ensure!(
        !handle.is_null(),
        "桌面/窗口站句柄为空: {}",
        std::io::Error::last_os_error()
    );
    let mut name = [0u16; 256];
    let mut needed = 0;
    checked(
        unsafe {
            GetUserObjectInformationW(
                handle,
                UOI_NAME,
                name.as_mut_ptr().cast(),
                size_of_val(&name) as u32,
                &mut needed,
            )
        },
        "读取桌面/窗口站名称",
    )?;
    Ok(String::from_utf16_lossy(
        &name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())],
    ))
}
pub(super) fn context() -> serde_json::Value {
    let value = |result: Result<String>| {
        result.map_or_else(
            |e| serde_json::json!({"error":format!("{e:#}")}),
            |v| serde_json::json!(v),
        )
    };
    let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    let input_name = object_name(input);
    let receives_input = (|| {
        let mut active = 0i32;
        checked(
            unsafe {
                GetUserObjectInformationW(
                    input,
                    UOI_IO,
                    (&mut active as *mut i32).cast(),
                    size_of::<i32>() as u32,
                    null_mut(),
                )
            },
            "读取桌面输入活动状态",
        )?;
        Ok::<_, anyhow::Error>(active != 0)
    })()
    .map_or_else(
        |e| serde_json::json!({"error":format!("{e:#}")}),
        |v| serde_json::json!(v),
    );
    if !input.is_null() {
        unsafe { CloseDesktop(input) };
    }
    let foreground = unsafe { GetForegroundWindow() };
    let mut foreground_process = 0;
    if !foreground.is_null() {
        unsafe { GetWindowThreadProcessId(foreground, &mut foreground_process) };
    }
    serde_json::json!({"thread":unsafe {GetCurrentThreadId()},"window_station":value(object_name(unsafe {GetProcessWindowStation()})),"thread_desktop":value(object_name(unsafe {GetThreadDesktop(GetCurrentThreadId())})),"input_desktop":value(input_name),"input_receives_input":receives_input,"foreground_present":!foreground.is_null(),"foreground_process":foreground_process})
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
            // Match Microsoft's desktop attachment sample. Opening with only
            // READOBJECTS/WRITEOBJECTS/SWITCHDESKTOP yields a restricted handle;
            // it does not establish all desktop access rights for the thread.
            // This requests access from the existing ACL; it never changes it.
            let next = OpenInputDesktop(0, 0, GENERIC_ALL);
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
                if self.name != name {
                    crate::diagnostics::log(&format!(
                        "Thread {} attached to desktop {name}; access=GENERIC_ALL",
                        GetCurrentThreadId()
                    ));
                }
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
