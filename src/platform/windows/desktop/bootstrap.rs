//! Transfer an already-authenticated pipe across sessions with DuplicateHandle.
//! CreateProcessAsUser cannot inherit handles from service session 0.
use super::sys::*;
use anyhow::{Result, ensure};
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Memory::*, Threading::*},
};
pub struct Mapping {
    _handle: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}
unsafe impl Send for Mapping {} // No pointers escape; used only by the owning thread.
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view);
        }
    }
}
impl Mapping {
    fn map(handle: OwnedHandle) -> Result<Self> {
        unsafe {
            let view = MapViewOfFile(
                handle.as_raw_handle(),
                FILE_MAP_READ | FILE_MAP_WRITE,
                0,
                0,
                size_of::<usize>(),
            );
            ensure!(
                !view.Value.is_null(),
                "映射桌面启动参数失败：{}",
                std::io::Error::last_os_error()
            );
            Ok(Self {
                _handle: handle,
                view,
            })
        }
    }
    pub fn create() -> Result<(String, Self)> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("生成桌面通道标识失败：{e}"))?;
        let name = format!(
            "Global\\LanDeskWorker-{}",
            random
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect::<String>()
        );
        let descriptor = Descriptor::new("D:P(A;;GA;;;SY)")?;
        unsafe {
            let handle = owned(
                CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    &descriptor.attributes(),
                    PAGE_READWRITE,
                    0,
                    size_of::<usize>() as u32,
                    wide(&name).as_ptr(),
                ),
                "创建桌面启动参数失败",
            )?;
            ensure!(GetLastError() != ERROR_ALREADY_EXISTS, "桌面启动标识冲突");
            Ok((name, Self::map(handle)?))
        }
    }
    pub fn transfer(&self, pipe: HANDLE, child: HANDLE) -> Result<()> {
        unsafe {
            let mut remote = ptr::null_mut();
            ok(
                DuplicateHandle(
                    GetCurrentProcess(),
                    pipe,
                    child,
                    &mut remote,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                ),
                "向桌面会话传递通道失败",
            )?;
            (&*self.view.Value.cast::<AtomicUsize>()).store(remote as usize, Ordering::Release);
        }
        Ok(())
    }
    pub fn receive(name: &str) -> Result<OwnedHandle> {
        ensure!(
            name.starts_with("Global\\LanDeskWorker-"),
            "桌面启动参数无效"
        );
        unsafe {
            let handle = owned(
                OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, wide(name).as_ptr()),
                "打开桌面启动参数失败",
            )?;
            let mapping = Self::map(handle)?;
            let handle = (&*mapping.view.Value.cast::<AtomicUsize>()).load(Ordering::Acquire);
            owned(handle as HANDLE, "桌面通道尚未传递")
        }
    }
}
