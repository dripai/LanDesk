//! Installs one persistent root-enumerated display adapter. No session management.
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::core::GUID;

const INSTANCE: &str = r"ROOT\LANDESKDISPLAY\0000";
const HARDWARE: &str = r"Root\MttVDD";
const DISPLAY_CLASS: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);

/// Resolve the adapter interface through SetupAPI; DISPLAY_DEVICE.DeviceID is
/// not a reliable device-instance identifier for an indirect display adapter.
pub(super) fn is_mtt_adapter(path: &[u16]) -> Result<bool> {
    let devices = Devices::new()?;
    let mut interface = SP_DEVICE_INTERFACE_DATA {
        cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
        ..Default::default()
    };
    check(
        unsafe { SetupDiOpenDeviceInterfaceW(devices.0, path.as_ptr(), 0, &mut interface) },
        "Open display adapter interface",
    )?;
    let mut needed = 0;
    unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            devices.0,
            &interface,
            ptr::null_mut(),
            0,
            &mut needed,
            ptr::null_mut(),
        );
    }
    ensure!(
        unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER && (8..=8192).contains(&needed),
        "Invalid adapter interface detail length"
    );
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    let detail = buffer
        .as_mut_ptr()
        .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    unsafe {
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
    }
    let mut adapter = device_data();
    check(
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                devices.0,
                &interface,
                detail,
                needed,
                ptr::null_mut(),
                &mut adapter,
            )
        },
        "Resolve display adapter device",
    )?;
    Ok(is_mtt(&hardware_ids(devices.0, &adapter)?))
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn check(result: i32, operation: &str) -> Result<()> {
    if result == 0 {
        return Err(std::io::Error::last_os_error()).context(operation.to_owned());
    }
    Ok(())
}

struct Devices(HDEVINFO);
impl Drop for Devices {
    fn drop(&mut self) {
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn device_data() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    }
}

impl Devices {
    fn new() -> Result<Self> {
        let handle = unsafe { SetupDiCreateDeviceInfoList(&DISPLAY_CLASS, ptr::null_mut()) };
        ensure!(
            handle != INVALID_HANDLE_VALUE as isize,
            "Create device list: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self(handle))
    }

    fn owned(&self) -> Result<Option<SP_DEVINFO_DATA>> {
        let mut data = device_data();
        if unsafe {
            SetupDiOpenDeviceInfoW(
                self.0,
                wide(INSTANCE).as_ptr(),
                ptr::null_mut(),
                0,
                &mut data,
            )
        } == 0
        {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_SUCH_DEVINST {
                return Ok(None);
            }
            bail!("Open LanDesk display: Windows error {error}");
        }
        ensure!(
            hardware_ids(self.0, &data)?
                .iter()
                .any(|id| id.eq_ignore_ascii_case(HARDWARE)),
            "LanDesk instance ID belongs to another driver; refusing to change it"
        );
        Ok(Some(data))
    }

    fn remove(&self, data: &SP_DEVINFO_DATA) -> Result<bool> {
        let params = SP_REMOVEDEVICE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_REMOVE,
            },
            Scope: DI_REMOVEDEVICE_GLOBAL,
            HwProfile: 0,
        };
        unsafe {
            check(
                SetupDiSetClassInstallParamsW(
                    self.0,
                    data,
                    &params.ClassInstallHeader,
                    size_of::<SP_REMOVEDEVICE_PARAMS>() as u32,
                ),
                "Set removal parameters",
            )?;
            check(
                SetupDiCallClassInstaller(DIF_REMOVE, self.0, data),
                "Remove LanDesk display",
            )?;
        }
        Ok(self.params(data)?.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0)
    }

    fn params(&self, data: &SP_DEVINFO_DATA) -> Result<SP_DEVINSTALL_PARAMS_W> {
        let mut params = SP_DEVINSTALL_PARAMS_W {
            cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
            ..Default::default()
        };
        check(
            unsafe { SetupDiGetDeviceInstallParamsW(self.0, data, &mut params) },
            "Get device install parameters",
        )?;
        Ok(params)
    }
}

fn hardware_ids(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Result<Vec<String>> {
    let mut bytes = 0;
    let mut kind = 0;
    unsafe {
        if SetupDiGetDeviceRegistryPropertyW(
            set,
            data,
            SPDRP_HARDWAREID,
            &mut kind,
            ptr::null_mut(),
            0,
            &mut bytes,
        ) == 0
        {
            let error = GetLastError();
            if error == ERROR_INVALID_DATA {
                return Ok(Vec::new());
            }
            ensure!(
                error == ERROR_INSUFFICIENT_BUFFER,
                "Read hardware IDs: Windows error {error}"
            );
        }
        ensure!(
            bytes > 0 && bytes <= 65536 && bytes % 2 == 0,
            "Invalid hardware ID buffer size"
        );
        let mut buffer = vec![0u16; bytes as usize / 2];
        check(
            SetupDiGetDeviceRegistryPropertyW(
                set,
                data,
                SPDRP_HARDWAREID,
                &mut kind,
                buffer.as_mut_ptr().cast(),
                bytes,
                ptr::null_mut(),
            ),
            "Read hardware IDs",
        )?;
        ensure!(kind == 7, "Hardware IDs must be REG_MULTI_SZ");
        Ok(buffer
            .split(|c| *c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect())
    }
}

fn is_mtt(ids: &[String]) -> bool {
    ids.iter()
        .any(|id| id.eq_ignore_ascii_case(HARDWARE) || id.eq_ignore_ascii_case("MttVDD"))
}
fn present_devices() -> Result<Vec<serde_json::Value>> {
    let raw = unsafe {
        SetupDiGetClassDevsW(&DISPLAY_CLASS, ptr::null(), ptr::null_mut(), DIGCF_PRESENT)
    };
    ensure!(
        raw != INVALID_HANDLE_VALUE as isize,
        "Enumerate display adapters: {}",
        std::io::Error::last_os_error()
    );
    let devices = Devices(raw);
    let mut result = Vec::new();
    for index in 0.. {
        let mut data = device_data();
        if unsafe { SetupDiEnumDeviceInfo(devices.0, index, &mut data) } == 0 {
            ensure!(
                unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS,
                "Enumerate display adapters: {}",
                std::io::Error::last_os_error()
            );
            break;
        }
        let ids = hardware_ids(devices.0, &data)?;
        if !is_mtt(&ids) {
            continue;
        }
        let mut instance = [0u16; 4096];
        check(
            unsafe {
                SetupDiGetDeviceInstanceIdW(
                    devices.0,
                    &data,
                    instance.as_mut_ptr(),
                    instance.len() as u32,
                    ptr::null_mut(),
                )
            },
            "Read display device instance",
        )?;
        let end = instance
            .iter()
            .position(|c| *c == 0)
            .context("Device instance lacks terminator")?;
        let instance = String::from_utf16(&instance[..end])?;
        let (started, problem) = started(&data)?;
        result.push(serde_json::json!({"owned": instance.eq_ignore_ascii_case(INSTANCE), "instance":instance,"hardware_ids":ids,"started":started,"problem":problem}));
    }
    Ok(result)
}
fn reject_inactive_existing(state: &serde_json::Value) -> Result<()> {
    ensure!(
        state["devices"]
            .as_array()
            .context("Missing display devices")?
            .is_empty()
            || state["available"] == true,
        "Existing MttVDD device is not running: {}",
        state["devices"]
    );
    Ok(())
}

fn started(data: &SP_DEVINFO_DATA) -> Result<(bool, u32)> {
    let mut flags = 0;
    let mut problem = 0;
    let result = unsafe { CM_Get_DevNode_Status(&mut flags, &mut problem, data.DevInst, 0) };
    ensure!(
        result == CR_SUCCESS,
        "Read device status: CONFIGRET {result}"
    );
    Ok((flags & DN_STARTED != 0 && problem == 0, problem))
}

pub(super) fn status() -> Result<serde_json::Value> {
    let present = present_devices()?;
    let devices = Devices::new()?;
    let owned = devices.owned()?;
    let (running, problem) = match &owned {
        Some(data) => started(data)?,
        None => (false, 0),
    };
    let available = present.iter().any(|device| device["started"] == true);
    Ok(
        serde_json::json!({"installed":owned.is_some(),"started":running,"problem":problem,"instance":INSTANCE,
        "available":available,"devices":present,"created":false,"reused":available}),
    )
}

pub(super) fn install(inf: &Path) -> Result<serde_json::Value> {
    let state = status()?;
    if state["available"] == true {
        return Ok(state);
    }
    reject_inactive_existing(&state)?;
    ensure!(
        state["installed"] != true,
        "LanDesk display is registered but not running (problem {}); inspect display status",
        state["problem"]
    );
    let devices = Devices::new()?;
    ensure!(
        inf.is_file(),
        "Missing bundled driver INF: {}",
        inf.display()
    );
    let path: Vec<u16> = inf.as_os_str().encode_wide().chain(Some(0)).collect();
    ensure!(
        path.len() <= 260,
        "Driver path exceeds Windows SetupAPI MAX_PATH"
    );
    let mut data = device_data();
    unsafe {
        check(
            SetupDiCreateDeviceInfoW(
                devices.0,
                wide(INSTANCE).as_ptr(),
                &DISPLAY_CLASS,
                wide("LanDesk Virtual Display").as_ptr(),
                ptr::null_mut(),
                0,
                &mut data,
            ),
            "Create LanDesk device",
        )?;
        let ids: Vec<u16> = wide(HARDWARE).into_iter().chain(Some(0)).collect();
        check(
            SetupDiSetDeviceRegistryPropertyW(
                devices.0,
                &mut data,
                SPDRP_HARDWAREID,
                ids.as_ptr().cast(),
                (ids.len() * 2) as u32,
            ),
            "Set display hardware ID",
        )?;
        check(
            SetupDiCallClassInstaller(DIF_REGISTERDEVICE, devices.0, &data),
            "Register LanDesk device",
        )?;
    }
    let result = (|| -> Result<_> {
        let mut params = devices.params(&data)?;
        params.Flags |= DI_ENUMSINGLEINF;
        params.DriverPath[..path.len()].copy_from_slice(&path);
        unsafe {
            check(
                SetupDiSetDeviceInstallParamsW(devices.0, &data, &params),
                "Select bundled INF",
            )?;
            check(
                SetupDiBuildDriverInfoList(devices.0, &mut data, SPDIT_COMPATDRIVER),
                "Build compatible driver list",
            )?;
            let mut driver = SP_DRVINFO_DATA_V2_W {
                cbSize: size_of::<SP_DRVINFO_DATA_V2_W>() as u32,
                ..Default::default()
            };
            check(
                SetupDiEnumDriverInfoW(devices.0, &data, SPDIT_COMPATDRIVER, 0, &mut driver),
                "Find bundled driver",
            )?;
            let mut reboot = 0;
            // Windows checks the original signed package. Never force an unsigned driver.
            check(
                DiInstallDevice(ptr::null_mut(), devices.0, &data, &driver, 0, &mut reboot),
                "Install bundled display driver",
            )?;
            if reboot != 0 || devices.params(&data)?.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0 {
                return Ok(
                    serde_json::json!({"installed": true, "reboot_required": true, "instance": INSTANCE}),
                );
            }
        }
        for _ in 0..50 {
            let (ready, _) = started(&data)?;
            if ready {
                return status();
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let (_, problem) = started(&data)?;
        bail!(
            "Display driver did not start (problem code {problem}); check Windows SetupAPI.dev.log"
        );
    })();
    match result {
        Ok(mut value) => {
            value["created"] = true.into();
            value["reused"] = false.into();
            Ok(value)
        }
        Err(error) => match devices.remove(&data) {
            Ok(reboot) => bail!(
                "{error:#}; device rolled back (reboot_required={reboot}); Windows may retain the verified package in Driver Store"
            ),
            Err(cleanup) => bail!(
                "{error:#}; rollback FAILED: {cleanup:#}; run --uninstall to remove {INSTANCE}"
            ),
        },
    }
}

pub fn run() -> Result<()> {
    ensure!(
        cfg!(target_arch = "x86_64"),
        "Bundled driver requires Windows x64"
    );
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [mode] if mode == "--status" => status()?,
        [mode] if mode == "--check-install" => {
            let state = status()?;
            reject_inactive_existing(&state)?;
            serde_json::json!({"install_allowed": true, "reuse_existing": state["available"]})
        }
        [mode] if mode == "--install" => {
            let exe = std::env::current_exe()?;
            install(
                &exe.parent()
                    .context("Executable has no parent directory")?
                    .join("driver/MttVDD.inf"),
            )?
        }
        [mode] if mode == "--uninstall" => uninstall()?,
        _ => bail!(
            "Use --status or --uninstall. Double-click LanDeskServer.exe for complete installation."
        ),
    };
    println!("{result}");
    Ok(())
}
pub(super) fn uninstall() -> Result<serde_json::Value> {
    let devices = Devices::new()?;
    let (removed, reboot) = match devices.owned()? {
        Some(data) => (true, devices.remove(&data)?),
        None => (false, false),
    };
    Ok(serde_json::json!({"removed": removed, "reboot_required": reboot, "instance": INSTANCE}))
}
