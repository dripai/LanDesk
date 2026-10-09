use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsStr,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    ptr,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::*,
    Security::*,
    System::{StationsAndDesktops::*, Threading::*},
    UI::Shell::*,
};
pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
pub fn ok(value: i32, operation: &str) -> Result<()> {
    if value == 0 {
        Err(std::io::Error::last_os_error()).context(operation.to_owned())
    } else {
        Ok(())
    }
}
pub unsafe fn owned(handle: HANDLE, operation: &str) -> Result<OwnedHandle> {
    ensure!(
        !handle.is_null() && handle != INVALID_HANDLE_VALUE,
        "{operation}：{}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
pub fn token(process: HANDLE, access: u32) -> Result<OwnedHandle> {
    let mut handle = ptr::null_mut();
    unsafe {
        ok(
            OpenProcessToken(process, access, &mut handle),
            "打开进程令牌失败",
        )?;
        owned(handle, "进程令牌")
    }
}
pub fn current_is_system() -> Result<bool> {
    let token = token(unsafe { GetCurrentProcess() }, TOKEN_QUERY)?;
    let mut length = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut length,
        );
    }
    let mut data = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        ok(
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                data.as_mut_ptr().cast(),
                length,
                &mut length,
            ),
            "查询运行身份失败",
        )?;
        let user = &*data.as_ptr().cast::<TOKEN_USER>();
        Ok(IsWellKnownSid(user.User.Sid, WinLocalSystemSid) != 0)
    }
}
pub fn enable_privilege(name: *const u16) -> Result<()> {
    let token = token(
        unsafe { GetCurrentProcess() },
        TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
    )?;
    let mut luid = LUID::default();
    unsafe {
        ok(
            LookupPrivilegeValueW(ptr::null(), name, &mut luid),
            "查询系统权限失败",
        )?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        SetLastError(0);
        ok(
            AdjustTokenPrivileges(
                token.as_raw_handle(),
                0,
                &privileges,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            "启用系统权限失败",
        )?;
        ensure!(
            GetLastError() != ERROR_NOT_ALL_ASSIGNED,
            "系统未授予所需权限"
        );
    }
    Ok(())
}
pub fn image_path(process: HANDLE) -> Result<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let mut length = buf.len() as u32;
    unsafe {
        ok(
            QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut length),
            "查询程序路径失败",
        )?;
    }
    Ok(std::ffi::OsString::from_wide(&buf[..length as usize]).into())
}
use std::os::windows::ffi::OsStringExt;
pub fn install_root() -> Result<PathBuf> {
    unsafe {
        let mut path = ptr::null_mut();
        let result = SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, ptr::null_mut(), &mut path);
        ensure!(result >= 0, "无法定位 Program Files：0x{result:08x}");
        let mut len = 0;
        while *path.add(len) != 0 {
            len += 1;
        }
        let result = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
            path, len,
        )))
        .join("LanDeskServer");
        windows_sys::Win32::System::Com::CoTaskMemFree(path.cast());
        Ok(result)
    }
}
pub struct Descriptor(pub *mut core::ffi::c_void);
impl Descriptor {
    pub fn new(sddl: &str) -> Result<Self> {
        let mut descriptor = ptr::null_mut();
        unsafe {
            ok(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide(sddl).as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                ),
                "创建服务访问规则失败",
            )?;
        }
        Ok(Self(descriptor))
    }
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
pub struct Desktop {
    handle: HDESK,
    original: HDESK,
    pub name: String,
}
impl Desktop {
    pub fn open() -> Result<Self> {
        unsafe {
            let handle = OpenInputDesktop(
                0,
                0,
                DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS | DESKTOP_SWITCHDESKTOP,
            );
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("打开 Windows 输入桌面失败");
            }
            let mut desktop = Self {
                handle,
                original: ptr::null_mut(),
                name: String::new(),
            };
            let mut buffer = [0u16; 256];
            let mut needed = 0;
            ok(
                GetUserObjectInformationW(
                    handle,
                    UOI_NAME,
                    buffer.as_mut_ptr().cast(),
                    std::mem::size_of_val(&buffer) as u32,
                    &mut needed,
                ),
                "查询输入桌面失败",
            )?;
            desktop.name = String::from_utf16_lossy(
                &buffer[..buffer.iter().position(|v| *v == 0).unwrap_or(buffer.len())],
            );
            Ok(desktop)
        }
    }
    pub fn bind(&mut self) -> Result<()> {
        unsafe {
            self.original = GetThreadDesktop(GetCurrentThreadId());
            ok(SetThreadDesktop(self.handle), "切换采集线程到当前桌面失败")
        }
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        unsafe {
            if !self.original.is_null() {
                SetThreadDesktop(self.original);
            }
            CloseDesktop(self.handle);
        }
    }
}
