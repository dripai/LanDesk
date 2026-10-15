//! Native installer resources: ACLs, registry snapshots, firewall, shortcuts and process cleanup.
use super::desktop::Handle;
use super::desktop::checked;
use super::desktop::wide;
use super::ui;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;
use std::ptr::null_mut;
use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::NetworkManagement::WindowsFirewall::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::core::BSTR;
use windows::core::Interface;
use windows::core::PCWSTR;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::WinTrust::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::*;

pub(super) const OWNER: &str = "LanDeskServer console service v1";
pub(super) const PRIVATE: &str = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
pub(super) const APP: &str =
    "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;LS)(A;OICI;0x1200a9;;;BU)";
pub(super) const TRANSPORT_ACL: &str =
    "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;LS)";
pub(super) const LOGS: &str = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";
pub(super) const RUN: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
pub(super) const OPTIONS: &str = r"SOFTWARE\LanDesk\Server";
pub(super) const UNINSTALL: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\LanDeskServer";
pub(super) const POLICY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
pub(super) const SAS: &str = "SoftwareSASGeneration";
pub(super) const FW_NAME: &str = "LanDesk-SSH-In-TCP";
const FW_GROUP: &str = "LanDesk managed transport";

pub(super) fn environment(name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os(name).with_context(|| format!("{name} missing"))?,
    ))
}
pub(super) fn plain(path: &Path) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "拒绝重解析路径: {}",
        path.display()
    );
    Ok(())
}
pub(super) fn plain_chain(path: &Path) -> Result<()> {
    for parent in path.ancestors() {
        if parent.exists() {
            plain(parent)?;
        }
    }
    Ok(())
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn descriptor(sddl: &str) -> Result<Descriptor> {
    let mut value = null_mut();
    checked(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(sddl).as_ptr(),
                1,
                &mut value,
                null_mut(),
            )
        },
        "创建访问控制描述符",
    )?;
    Ok(Descriptor(value))
}
#[derive(Clone)]
pub(super) struct Security(Vec<usize>);
struct RestorePrivilege {
    token: Handle,
    previous: TOKEN_PRIVILEGES,
}
impl RestorePrivilege {
    fn new() -> Result<Self> {
        let mut token = null_mut();
        checked(
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                    &mut token,
                )
            },
            "打开权限恢复令牌",
        )?;
        let token = Handle::new(token, "权限恢复令牌")?;
        let mut luid = LUID::default();
        checked(
            unsafe {
                LookupPrivilegeValueW(
                    std::ptr::null(),
                    wide("SeRestorePrivilege").as_ptr(),
                    &mut luid,
                )
            },
            "读取权限恢复特权",
        )?;
        let requested = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut previous = TOKEN_PRIVILEGES::default();
        let mut needed = 0;
        unsafe {
            SetLastError(0);
        }
        checked(
            unsafe {
                AdjustTokenPrivileges(
                    token.0,
                    0,
                    &requested,
                    std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
                    &mut previous,
                    &mut needed,
                )
            },
            "启用权限恢复特权",
        )?;
        ensure!(
            unsafe { GetLastError() } == ERROR_SUCCESS,
            "当前管理员缺少 SeRestorePrivilege，原目录权限无法恢复"
        );
        Ok(Self { token, previous })
    }
}
impl Drop for RestorePrivilege {
    fn drop(&mut self) {
        if unsafe {
            AdjustTokenPrivileges(self.token.0, 0, &self.previous, 0, null_mut(), null_mut())
        } == 0
        {
            crate::diagnostics::log(&format!(
                "恢复安装进程特权状态失败: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
}
impl Security {
    pub(super) fn read(path: &Path) -> Result<Self> {
        plain_chain(path)?;
        let name = wide(&path.to_string_lossy());
        let mut needed = 0;
        unsafe {
            GetFileSecurityW(
                name.as_ptr(),
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                null_mut(),
                0,
                &mut needed,
            );
        }
        ensure!(
            unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER && (1..=65536).contains(&needed),
            "读取权限长度失败: {}",
            path.display()
        );
        let mut data = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        checked(
            unsafe {
                GetFileSecurityW(
                    name.as_ptr(),
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    data.as_mut_ptr().cast(),
                    needed,
                    &mut needed,
                )
            },
            "读取路径权限",
        )?;
        Ok(Self(data))
    }
    pub(super) fn restore(&self, path: &Path) -> Result<()> {
        plain_chain(path)?;
        let _privilege = RestorePrivilege::new()?;
        let mut control = 0;
        let mut revision = 0;
        checked(
            unsafe {
                GetSecurityDescriptorControl(self.0.as_ptr() as _, &mut control, &mut revision)
            },
            "读取权限继承状态",
        )?;
        checked(
            unsafe {
                SetFileSecurityW(
                    wide(&path.to_string_lossy()).as_ptr(),
                    OWNER_SECURITY_INFORMATION
                        | DACL_SECURITY_INFORMATION
                        | if control & SE_DACL_PROTECTED != 0 {
                            PROTECTED_DACL_SECURITY_INFORMATION
                        } else {
                            UNPROTECTED_DACL_SECURITY_INFORMATION
                        },
                    self.0.as_ptr() as _,
                )
            },
            "恢复路径权限",
        )
    }
    fn trusted(&self, readers: &[&str]) -> Result<()> {
        let pointer = self.0.as_ptr() as _;
        let mut owner = null_mut();
        let mut defaulted = 0;
        let mut control = 0;
        let mut revision = 0;
        checked(
            unsafe { GetSecurityDescriptorOwner(pointer, &mut owner, &mut defaulted) },
            "读取权限所有者",
        )?;
        let owner = sid_string(owner)?;
        ensure!(
            owner == "S-1-5-18" || owner == "S-1-5-32-544",
            "目录所有者不可信"
        );
        checked(
            unsafe { GetSecurityDescriptorControl(pointer, &mut control, &mut revision) },
            "读取 ACL 状态",
        )?;
        ensure!(control & SE_DACL_PROTECTED != 0, "目录权限未隔离继承");
        let mut acl = null_mut();
        let mut present = 0;
        checked(
            unsafe { GetSecurityDescriptorDacl(pointer, &mut present, &mut acl, &mut defaulted) },
            "读取目录 ACL",
        )?;
        ensure!(present != 0 && !acl.is_null(), "目录缺少 ACL");
        for index in 0..unsafe { (*acl).AceCount } as u32 {
            let mut ace = null_mut();
            checked(unsafe { GetAce(acl, index, &mut ace) }, "读取 ACL 条目")?;
            let kind = unsafe { (*ace.cast::<ACE_HEADER>()).AceType };
            if kind == 1 {
                continue;
            } // Deny ACE cannot grant additional access.
            ensure!(kind == 0, "目录包含不支持的授权 ACL 条目");
            let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = sid_string((&allowed.SidStart as *const u32) as _)?;
            if sid != "S-1-5-18" && sid != "S-1-5-32-544" {
                ensure!(
                    readers.contains(&sid.as_str()) && allowed.Mask & !0x1200a9 == 0,
                    "目录向其他用户授予了写权限"
                );
            }
        }
        Ok(())
    }
}
fn sid_string(sid: PSID) -> Result<String> {
    let mut text = null_mut();
    checked(
        unsafe { ConvertSidToStringSidW(sid, &mut text) },
        "读取 SID",
    )?;
    let result = unsafe { super::setup_services::string(text) };
    unsafe {
        LocalFree(text.cast());
    }
    result
}
pub(super) fn protect(path: &Path, sddl: &str) -> Result<()> {
    plain_chain(path)?;
    let descriptor = descriptor(sddl)?;
    // Unlike SetNamedSecurityInfo, this supported non-recursive API leaves existing
    // third-party driver directories' child ACLs intact (documented SetFileSecurity behavior).
    checked(
        unsafe {
            SetFileSecurityW(
                wide(&path.to_string_lossy()).as_ptr(),
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.0,
            )
        },
        "设置路径权限",
    )
    .with_context(|| path.display().to_string())
}
pub(super) fn create_directory(path: &Path, sddl: &str) -> Result<()> {
    plain_chain(path)?;
    let descriptor = descriptor(sddl)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    checked(
        unsafe { CreateDirectoryW(wide(&path.to_string_lossy()).as_ptr(), &attributes) },
        "创建受保护目录",
    )
    .with_context(|| path.display().to_string())
}
pub(super) fn owned(path: &Path, readers: &[&str]) -> Result<()> {
    ensure!(path.is_dir(), "安装目录缺失: {}", path.display());
    Security::read(path)?.trusted(readers)?;
    let marker = path.join("landesk-owner.txt");
    plain(&marker)?;
    ensure!(
        std::fs::read_to_string(&marker)? == OWNER,
        "目录不属于 LanDesk: {}",
        path.display()
    );
    Ok(())
}
pub(super) fn ensure_directory(path: &Path, sddl: &str, readers: &[&str]) -> Result<()> {
    if path.exists() {
        owned(path, readers)?;
    } else {
        create_directory(path, sddl)?;
        std::fs::write(path.join("landesk-owner.txt"), OWNER)?;
    }
    Ok(())
}
pub(super) fn read(path: &str, name: &str) -> Result<Option<(u32, Vec<u8>)>> {
    ui::value(HKEY_LOCAL_MACHINE, path, name)
}
pub(super) fn text(value: &str) -> (u32, Vec<u8>) {
    (
        REG_SZ,
        wide(value).into_iter().flat_map(u16::to_le_bytes).collect(),
    )
}
pub(super) fn number(value: u32) -> (u32, Vec<u8>) {
    (REG_DWORD, value.to_le_bytes().to_vec())
}
pub(super) fn decode(value: &(u32, Vec<u8>)) -> Result<String> {
    ensure!(
        (value.0 == REG_SZ || value.0 == REG_EXPAND_SZ) && value.1.len().is_multiple_of(2),
        "注册表字符串无效"
    );
    let mut buffer: Vec<u16> = value
        .1
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    ensure!(buffer.pop() == Some(0), "注册表字符串缺少终止符");
    ensure!(!buffer.contains(&0), "注册表字符串包含嵌入终止符");
    Ok(String::from_utf16(&buffer)?)
}
pub(super) fn put(path: &str, name: &str, value: Option<&(u32, Vec<u8>)>) -> Result<()> {
    ui::put(&ui::key(path)?, name, value)
}
fn registry_exists(path: &str) -> Result<bool> {
    let mut key = null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(path).as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    ensure!(result == ERROR_SUCCESS, "读取注册表键失败 ({result})");
    unsafe {
        RegCloseKey(key);
    }
    Ok(true)
}
pub(super) fn delete_key(path: &str) -> Result<()> {
    let result =
        unsafe { RegDeleteKeyExW(HKEY_LOCAL_MACHINE, wide(path).as_ptr(), KEY_WOW64_64KEY, 0) };
    ensure!(
        result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND,
        "删除注册表键失败 ({result})"
    );
    Ok(())
}
type RegistryValue = Option<(u32, Vec<u8>)>;
pub(super) struct RegistrySnapshot {
    path: String,
    existed: bool,
    values: Vec<(String, RegistryValue)>,
}
impl RegistrySnapshot {
    pub(super) fn read(path: &str, names: &[&str]) -> Result<Self> {
        Ok(Self {
            path: path.into(),
            existed: registry_exists(path)?,
            values: names
                .iter()
                .map(|name| Ok(((*name).into(), read(path, name)?)))
                .collect::<Result<_>>()?,
        })
    }
    pub(super) fn restore(self) -> Result<()> {
        if !self.existed {
            return delete_key(&self.path);
        }
        for (name, value) in self.values {
            put(&self.path, &name, value.as_ref())?;
        }
        Ok(())
    }
}
pub(super) fn uninstall_snapshot(destination: &Path) -> Result<RegistrySnapshot> {
    if registry_exists(UNINSTALL)? {
        ensure!(
            read(UNINSTALL, "InstallLocation")?
                .map(|v| decode(&v))
                .transpose()?
                .as_deref()
                == Some(destination.to_string_lossy().as_ref())
                && read(UNINSTALL, "DisplayName")?
                    .map(|v| decode(&v))
                    .transpose()?
                    .as_deref()
                    == Some("LanDeskServer"),
            "同名卸载入口不属于 LanDesk"
        );
    }
    RegistrySnapshot::read(
        UNINSTALL,
        &[
            "DisplayName",
            "DisplayVersion",
            "Publisher",
            "InstallLocation",
            "DisplayIcon",
            "UninstallString",
            "NoModify",
            "NoRepair",
        ],
    )
}
pub(super) fn register_uninstall(destination: &Path) -> Result<()> {
    let executable = destination.join("LanDeskServer.exe");
    for (name, value) in [
        ("DisplayName", "LanDeskServer".into()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").into()),
        ("Publisher", "LanDesk".into()),
        ("InstallLocation", destination.display().to_string()),
        ("DisplayIcon", executable.display().to_string()),
        (
            "UninstallString",
            format!("\"{}\" --uninstall", executable.display()),
        ),
    ] {
        put(UNINSTALL, name, Some(&text(&value)))?;
    }
    for name in ["NoModify", "NoRepair"] {
        put(UNINSTALL, name, Some(&number(1)))?;
    }
    Ok(())
}

pub(super) struct Com;
impl Com {
    pub(super) fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
fn firewall_rules() -> Result<INetFwRules> {
    unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)?;
        Ok(policy.Rules()?)
    }
}
pub(super) fn firewall(executable: &Path) -> Result<Option<INetFwRule>> {
    match unsafe { firewall_rules()?.Item(&BSTR::from(FW_NAME)) } {
        Ok(rule) => {
            ensure!(
                unsafe { rule.Grouping()? } == FW_GROUP
                    && unsafe { rule.ApplicationName()? }
                        .to_string()
                        .eq_ignore_ascii_case(&executable.to_string_lossy()),
                "同名防火墙规则不属于 LanDesk"
            );
            Ok(Some(rule))
        }
        Err(error) if error.code().0 as u32 == 0x80070002 => Ok(None),
        Err(error) => Err(error).context("读取防火墙规则"),
    }
}
pub(super) fn add_firewall(executable: &Path, port: u16) -> Result<()> {
    unsafe {
        let rule: INetFwRule = CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER)?;
        rule.SetName(&BSTR::from(FW_NAME))?;
        rule.SetDescription(&BSTR::from("LanDesk encrypted desktop"))?;
        rule.SetGrouping(&BSTR::from(FW_GROUP))?;
        rule.SetApplicationName(&BSTR::from(executable.to_string_lossy().as_ref()))?;
        rule.SetProtocol(NET_FW_IP_PROTOCOL_TCP.0)?;
        rule.SetLocalPorts(&BSTR::from(port.to_string()))?;
        rule.SetDirection(NET_FW_RULE_DIR_IN)?;
        rule.SetProfiles(NET_FW_PROFILE2_ALL.0)?;
        rule.SetAction(NET_FW_ACTION_ALLOW)?;
        rule.SetEnabled(VARIANT_TRUE)?;
        firewall_rules()?.Add(&rule)?;
    }
    Ok(())
}
pub(super) fn remove_firewall() -> Result<()> {
    unsafe {
        firewall_rules()?.Remove(&BSTR::from(FW_NAME))?;
    }
    Ok(())
}
pub(super) fn shortcut_path() -> Result<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_CommonPrograms, KNOWN_FOLDER_FLAG(0), None)?;
        let output = PathBuf::from(path.to_string()?);
        CoTaskMemFree(Some(path.0.cast()));
        Ok(output.join("LanDeskServer.lnk"))
    }
}
pub(super) fn shortcut_owned(path: &Path, executable: &Path) -> Result<()> {
    plain_chain(path)?;
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        file.Load(PCWSTR(wide(&path.to_string_lossy()).as_ptr()), STGM_READ)?;
        let mut target = [0u16; 32768];
        link.GetPath(&mut target, null_mut(), SLGP_RAWPATH.0 as u32)?;
        ensure!(
            super::setup_services::string(target.as_ptr())?
                .eq_ignore_ascii_case(&executable.to_string_lossy()),
            "同名快捷方式不属于 LanDesk"
        );
    }
    Ok(())
}
pub(super) fn create_shortcut(path: &Path, executable: &Path) -> Result<()> {
    plain_chain(path)?;
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(wide(&executable.to_string_lossy()).as_ptr()))?;
        link.SetArguments(PCWSTR(wide("--settings").as_ptr()))?;
        let file: IPersistFile = link.cast()?;
        file.Save(PCWSTR(wide(&path.to_string_lossy()).as_ptr()), true)?;
    }
    Ok(())
}
pub(super) fn verify_catalog(path: &Path) -> Result<()> {
    let name = wide(&path.to_string_lossy());
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: name.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let result = unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
    }
    ensure!(result == 0, "驱动签名信任校验失败: 0x{:08x}", result as u32);
    Ok(())
}
pub(super) fn close_installed_processes(executable: &Path, launcher: u32) -> Result<()> {
    // The management UI can run from the downloaded copy rather than Program
    // Files. Match its bytes to the owned installation before closing it; a
    // matching filename alone must never authorize terminating a process.
    let installed_bytes = match std::fs::read(executable) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("读取已安装程序以识别旧界面"),
    };
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    ensure!(
        raw != INVALID_HANDLE_VALUE,
        "枚举程序进程失败: {}",
        std::io::Error::last_os_error()
    );
    let snapshot = Handle::new(raw, "枚举程序进程")?;
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    while found != 0 {
        if entry.th32ProcessID != std::process::id()
            && entry.th32ProcessID != launcher
            && unsafe { super::setup_services::string(entry.szExeFile.as_ptr())? }
                .eq_ignore_ascii_case("LanDeskServer.exe")
        {
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION
                        | PROCESS_TERMINATE
                        | SYNCHRONIZATION_SYNCHRONIZE,
                    0,
                    entry.th32ProcessID,
                )
            };
            if raw.is_null() {
                ensure!(
                    unsafe { GetLastError() } == ERROR_INVALID_PARAMETER,
                    "无法检查 LanDesk 进程 {}: {}",
                    entry.th32ProcessID,
                    std::io::Error::last_os_error()
                );
            } else {
                let handle = Handle::new(raw, "打开 LanDesk 进程")?;
                let mut path = [0u16; 32768];
                let mut length = path.len() as u32;
                checked(
                    unsafe {
                        QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut length)
                    },
                    "读取进程路径",
                )?;
                let process_path = String::from_utf16(&path[..length as usize])?;
                let same_path = process_path.eq_ignore_ascii_case(&executable.to_string_lossy());
                let same_copy = if !same_path && let Some(bytes) = &installed_bytes {
                    std::fs::read(&process_path)
                        .with_context(|| format!("检查旧界面程序: {process_path}"))?
                        == *bytes
                } else {
                    false
                };
                if same_path || same_copy {
                    crate::diagnostics::log(&format!(
                        "关闭旧 LanDesk 进程 pid={} path={process_path}",
                        entry.th32ProcessID
                    ));
                    checked(
                        unsafe { TerminateProcess(handle.0, 0) },
                        "关闭已安装的 LanDesk 界面",
                    )?;
                    ensure!(
                        unsafe { WaitForSingleObject(handle.0, 20000) } == WAIT_OBJECT_0,
                        "已安装的程序退出超时"
                    );
                }
            }
        }
        found = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    ensure!(
        unsafe { GetLastError() } == ERROR_NO_MORE_FILES,
        "进程枚举未完成"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires Windows administrator; creates and removes a unique test registry key"]
    fn native_setup_registry_restores_existing_values_and_removes_new_key() -> Result<()> {
        let path = format!(r"SOFTWARE\LanDesk\NativeSetupTest{}", std::process::id());
        ensure!(
            !registry_exists(&path)?,
            "Test key already exists; nothing changed"
        );
        let fresh = RegistrySnapshot::read(&path, &["Version"])?;
        let result = (|| {
            put(&path, "Version", Some(&text("中文旧值")))?;
            put(&path, "Unrelated", Some(&number(17)))?;
            let existing = RegistrySnapshot::read(&path, &["Version", "NewValue"])?;
            put(&path, "Version", Some(&number(99)))?;
            put(&path, "NewValue", Some(&text("new")))?;
            existing.restore()?;
            ensure!(
                read(&path, "Version")? == Some(text("中文旧值"))
                    && read(&path, "NewValue")?.is_none()
                    && read(&path, "Unrelated")? == Some(number(17)),
                "Registry rollback mismatch"
            );
            Ok(())
        })();
        fresh.restore()?;
        ensure!(!registry_exists(&path)?, "Fresh key was not removed");
        result
    }
    #[test]
    #[ignore = "Requires Windows administrator; temporarily adds/removes only the LanDesk firewall rule"]
    fn native_setup_firewall_and_shortcut_roundtrip() -> Result<()> {
        let _com = Com::new()?;
        let path =
            environment("ProgramFiles")?.join(format!(".LanDeskApiTest-{}", std::process::id()));
        ensure!(
            !path.exists(),
            "Test directory already exists; nothing changed"
        );
        create_directory(&path, PRIVATE)?;
        let exe = path.join("LanDeskServer.exe");
        let shortcut = path.join("test.lnk");
        let result = (|| {
            ensure!(
                firewall(&exe)?.is_none(),
                "Existing rule retained; test refused to modify it"
            );
            create_shortcut(&shortcut, &exe)?;
            shortcut_owned(&shortcut, &exe)?;
            add_firewall(&exe, 65535)?;
            let result = (|| {
                let rule = firewall(&exe)?.context("Rule missing after Add")?;
                ensure!(
                    unsafe { rule.Protocol()? } == NET_FW_IP_PROTOCOL_TCP.0
                        && unsafe { rule.LocalPorts()? } == "65535"
                        && unsafe { rule.Direction()? } == NET_FW_RULE_DIR_IN
                        && unsafe { rule.Enabled()? } == VARIANT_TRUE
                        && unsafe { rule.Profiles()? } == NET_FW_PROFILE2_ALL.0,
                    "Firewall rule mismatch"
                );
                Ok(())
            })();
            remove_firewall()?;
            result
        })();
        std::fs::remove_dir_all(&path)?;
        result
    }
}
