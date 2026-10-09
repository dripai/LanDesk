//! The signed upstream UMDF driver is staged once by the elevated installer.
//! A SYSTEM worker owns a software device only when DXGI has no attached output.
use super::sys::{install_root, ok, wide};
use anyhow::{Context, Result, ensure};
use std::{ffi::c_void, io::Write, path::Path, ptr, sync::mpsc, time::Duration};
use windows_sys::Win32::{
    Devices::{DeviceAndDriverInstallation::*, Enumeration::Pnp::*},
    Foundation::*,
    System::Registry::*,
};

const KEY: &str = r"SOFTWARE\MikeTheTech\VirtualDisplayDriver";
const PATH_VALUE: &str = "VDDPATH";
const FILES: &[(&str, &[u8])] = &[
    (
        "MttVDD.inf",
        include_bytes!(concat!(env!("OUT_DIR"), "/MttVDD.inf")),
    ),
    (
        "MttVDD.dll",
        include_bytes!(concat!(env!("OUT_DIR"), "/MttVDD.dll")),
    ),
    (
        "mttvdd.cat",
        include_bytes!(concat!(env!("OUT_DIR"), "/mttvdd.cat")),
    ),
    (
        "vdd_settings.xml",
        include_bytes!("../../../../third_party/virtual-display/settings.xml"),
    ),
    (
        "LICENSE",
        include_bytes!("../../../../third_party/virtual-display/LICENSE"),
    ),
];

pub fn install() -> Result<()> {
    let directory = install_root()?.join("virtual-display-25.7.23");
    super::install::directory(&directory)?;
    for (name, bytes) in FILES {
        let path = directory.join(name);
        if path.exists() {
            if *name == "vdd_settings.xml" {
                ensure!(
                    path.is_file(),
                    "虚拟显示配置不是普通文件：{}",
                    path.display()
                );
                continue;
            }
            ensure!(
                std::fs::read(&path)? == *bytes,
                "虚拟显示驱动文件校验失败：{}",
                path.display()
            );
        } else {
            let mut file = tempfile::NamedTempFile::new_in(&directory)?;
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(&path)
                .context("保存内置虚拟显示驱动失败")?;
        }
    }
    // Stage the signed package without replacing drivers on someone else's devices.
    // SetupAPI enforces the operating system's catalog/driver signature policy.
    unsafe {
        ok(
            SetupCopyOEMInfW(
                wide(directory.join("MttVDD.inf")).as_ptr(),
                ptr::null(),
                SPOST_PATH,
                0,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            "安装内置虚拟显示驱动失败，请查看 Windows 驱动安装日志 %windir%\\inf\\setupapi.dev.log",
        )?;
    }
    configure(&directory).context("虚拟显示驱动已暂存，但配置未完成；重新运行 LanDeskServer 可重试")
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn registry_result(code: u32) -> Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(code as i32).into())
    }
}
fn configure(directory: &Path) -> Result<()> {
    unsafe {
        let mut key = ptr::null_mut();
        registry_result(RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(KEY).as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        ))?;
        let key = Key(key);
        let mut size = 0;
        let result = RegGetValueW(
            key.0,
            ptr::null(),
            wide(PATH_VALUE).as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        );
        // This upstream setting is global, not per-device. Keep an existing VDD
        // installation/configuration intact (including its resolution preferences).
        if result == ERROR_SUCCESS {
            return Ok(());
        }
        registry_result(if result == ERROR_FILE_NOT_FOUND {
            ERROR_SUCCESS
        } else {
            result
        })?;
        if Path::new(r"C:\VirtualDisplayDriver\vdd_settings.xml").try_exists()? {
            return Ok(());
        }
        let path = wide(directory);
        ensure!(
            path.len() <= 260,
            "虚拟显示驱动配置路径超过上游 MAX_PATH 限制"
        );
        registry_result(RegSetValueExW(
            key.0,
            wide(PATH_VALUE).as_ptr(),
            0,
            REG_SZ,
            path.as_ptr().cast(),
            (path.len() * 2) as u32,
        ))?;
    }
    Ok(())
}

pub struct Device {
    handle: HSWDEVICE,
    instance: Vec<u16>,
}
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            SwDeviceClose(self.handle);
        }
    }
}
type Creation = (i32, Vec<u16>);
unsafe extern "system" fn created(
    _: HSWDEVICE,
    result: i32,
    context: *const c_void,
    instance: *const u16,
) {
    // Ownership passes to the callback, so even a late completion after a timeout
    // cannot access a destroyed worker stack or a dropped receiver.
    let sender = unsafe { Box::from_raw(context.cast_mut().cast::<mpsc::SyncSender<Creation>>()) };
    let mut id = Vec::new();
    if !instance.is_null() {
        let mut offset = 0;
        unsafe {
            while *instance.add(offset) != 0 {
                id.push(*instance.add(offset));
                offset += 1;
            }
        }
    }
    id.push(0);
    let _ = sender.send((result, id));
}
impl Device {
    pub fn create() -> Result<Self> {
        let enumerator = wide("LanDeskVirtualDisplay");
        let instance = wide("Desktop");
        let hardware = wide("MttVDD\0"); // MULTI_SZ: two trailing NULs.
        let description = wide("LanDesk virtual display");
        let info = SW_DEVICE_CREATE_INFO {
            cbSize: size_of::<SW_DEVICE_CREATE_INFO>() as u32,
            pszInstanceId: instance.as_ptr(),
            pszzHardwareIds: hardware.as_ptr(),
            CapabilityFlags: (SWDeviceCapabilitiesDriverRequired
                | SWDeviceCapabilitiesSilentInstall) as u32,
            pszDeviceDescription: description.as_ptr(),
            ..Default::default()
        };
        let (sender, receiver) = mpsc::sync_channel::<Creation>(1);
        let context = Box::into_raw(Box::new(sender));
        let mut handle = ptr::null_mut();
        let result = unsafe {
            SwDeviceCreate(
                enumerator.as_ptr(),
                wide(r"HTREE\ROOT\0").as_ptr(),
                &info,
                0,
                ptr::null(),
                Some(created),
                context.cast(),
                &mut handle,
            )
        };
        if result < 0 {
            unsafe {
                drop(Box::from_raw(context));
            }
            anyhow::bail!("创建虚拟显示器失败：HRESULT 0x{:08X}", result as u32);
        }
        let mut device = Self {
            handle,
            instance: Vec::new(),
        };
        let (result, instance) = receiver
            .recv_timeout(Duration::from_secs(15))
            .context("Windows 未完成虚拟显示设备枚举")?;
        ensure!(
            result >= 0,
            "虚拟显示器枚举失败：HRESULT 0x{:08X}",
            result as u32
        );
        device.instance = instance;
        Ok(device)
    }
    pub fn diagnostic(&self) -> String {
        unsafe {
            let mut node = 0;
            let result =
                CM_Locate_DevNodeW(&mut node, self.instance.as_ptr(), CM_LOCATE_DEVNODE_NORMAL);
            if result != CR_SUCCESS {
                return format!("设备节点不可用：CONFIGRET 0x{result:08X}");
            }
            let (mut status, mut problem) = (0, 0);
            let result = CM_Get_DevNode_Status(&mut status, &mut problem, node, 0);
            if result != CR_SUCCESS {
                return format!("无法读取设备状态：CONFIGRET 0x{result:08X}");
            }
            format!("Windows 设备问题代码 {problem}，状态 0x{status:08X}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_callback_copies_the_os_instance_id() {
        let (sender, receiver) = mpsc::sync_channel::<Creation>(1);
        let context = Box::into_raw(Box::new(sender));
        let id = wide(r"SWD\LanDeskVirtualDisplay\Desktop");
        unsafe {
            created(ptr::null_mut(), 0, context.cast(), id.as_ptr());
        }
        drop(id);
        let (result, owned_id) = receiver.recv().unwrap();
        assert_eq!(result, 0);
        assert_eq!(owned_id, wide(r"SWD\LanDeskVirtualDisplay\Desktop"));
    }

    #[test]
    fn failed_callback_can_complete_after_receiver_is_gone() {
        let (sender, receiver) = mpsc::sync_channel::<Creation>(1);
        let context = Box::into_raw(Box::new(sender));
        drop(receiver);
        unsafe {
            created(ptr::null_mut(), -1, context.cast(), ptr::null());
        }
    }
}
