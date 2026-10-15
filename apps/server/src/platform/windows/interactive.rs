//! Console process identity and read-only diagnostics; never connects an RDP session.
use super::desktop::Handle;
use super::desktop::checked;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use serde_json::Value;
use serde_json::json;
use std::mem::offset_of;
use std::mem::size_of;
use std::ptr::null;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::RemoteDesktop::*;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::SystemServices::SE_GROUP_LOGON_ID;
use windows_sys::Win32::System::Threading::*;

fn text(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())])
}
fn process_session(pid: u32) -> Result<u32> {
    let mut session = 0;
    checked(
        unsafe { ProcessIdToSessionId(pid, &mut session) },
        "ProcessIdToSessionId",
    )?;
    Ok(session)
}
fn open_token(process: HANDLE, access: u32) -> Result<Handle> {
    let mut token = null_mut();
    checked(
        unsafe { OpenProcessToken(process, access, &mut token) },
        "OpenProcessToken",
    )?;
    Handle::new(token, "Process token")
}
fn scalar(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<u32> {
    let mut value = 0;
    let mut needed = 0;
    checked(
        unsafe {
            GetTokenInformation(
                token,
                class,
                (&mut value as *mut u32).cast(),
                size_of::<u32>() as u32,
                &mut needed,
            )
        },
        "GetTokenInformation scalar",
    )?;
    ensure!(needed == size_of::<u32>() as u32, "令牌标量长度无效");
    Ok(value)
}
fn token_buffer(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<(Vec<usize>, usize)> {
    let mut needed = 0;
    let ok = unsafe { GetTokenInformation(token, class, null_mut(), 0, &mut needed) };
    let error = unsafe { GetLastError() };
    ensure!(
        ok == 0 && error == ERROR_INSUFFICIENT_BUFFER && needed > 0,
        "读取令牌大小失败: {}",
        std::io::Error::from_raw_os_error(error as i32)
    );
    // usize storage keeps TOKEN_USER/GROUPS/MANDATORY_LABEL pointer fields aligned.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    let bytes = needed;
    checked(
        unsafe {
            GetTokenInformation(token, class, buffer.as_mut_ptr().cast(), bytes, &mut needed)
        },
        "GetTokenInformation",
    )?;
    ensure!(needed <= bytes, "令牌数据长度无效");
    Ok((buffer, needed as usize))
}
fn system_token(token: HANDLE) -> Result<bool> {
    let (data, bytes) = token_buffer(token, TokenUser)?;
    ensure!(bytes >= size_of::<TOKEN_USER>(), "令牌身份长度无效");
    let sid = unsafe { (*data.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    Ok(unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } != 0)
}
fn sid_text(sid: PSID) -> Result<String> {
    let mut raw = null_mut();
    checked(
        unsafe { ConvertSidToStringSidW(sid, &mut raw) },
        "ConvertSidToStringSidW",
    )?;
    let result = unsafe {
        let mut len = 0;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
        LocalFree(raw.cast());
        result
    };
    Ok(result)
}
pub(super) fn token_report(token: HANDLE) -> Result<Value> {
    let (label, bytes) = token_buffer(token, TokenIntegrityLevel)?;
    ensure!(
        bytes >= size_of::<TOKEN_MANDATORY_LABEL>(),
        "完整性标签长度无效"
    );
    let sid = unsafe { (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid };
    ensure!(unsafe { IsValidSid(sid) } != 0, "完整性 SID 无效");
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    ensure!(count > 0, "完整性 SID 没有 RID");
    let integrity = unsafe { *GetSidSubAuthority(sid, u32::from(count - 1)) };
    let (groups, bytes) = token_buffer(token, TokenGroups)?;
    let offset = offset_of!(TOKEN_GROUPS, Groups);
    ensure!(bytes >= offset, "令牌组长度无效");
    let count = unsafe { *groups.as_ptr().cast::<u32>() } as usize;
    ensure!(
        count <= (bytes - offset) / size_of::<SID_AND_ATTRIBUTES>(),
        "令牌组数量无效"
    );
    let groups = unsafe {
        std::slice::from_raw_parts(
            groups
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    };
    let logon_sids = groups
        .iter()
        .filter(|group| group.Attributes & SE_GROUP_LOGON_ID as u32 == SE_GROUP_LOGON_ID as u32)
        .map(|group| sid_text(group.Sid))
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"system":system_token(token)?,"session":scalar(token, TokenSessionId)?,"ui_access":scalar(token, TokenUIAccess)? != 0,"integrity_rid":integrity,"logon_sids":logon_sids}),
    )
}
struct Process {
    pid: u32,
    name: String,
}
fn desktop_processes() -> Result<Vec<Process>> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    ensure!(
        raw != INVALID_HANDLE_VALUE,
        "CreateToolhelp32Snapshot: {}",
        std::io::Error::last_os_error()
    );
    let snapshot = Handle::new(raw, "Process snapshot")?;
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut ok = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    let mut result = Vec::new();
    while ok != 0 {
        let name = text(&entry.szExeFile);
        if ["winlogon.exe", "LogonUI.exe", "LanDeskServer.exe"]
            .iter()
            .any(|value| name.eq_ignore_ascii_case(value))
        {
            result.push(Process {
                pid: entry.th32ProcessID,
                name,
            });
        }
        ok = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    let error = unsafe { GetLastError() };
    ensure!(
        error == ERROR_NO_MORE_FILES,
        "枚举桌面进程失败: {}",
        std::io::Error::from_raw_os_error(error as i32)
    );
    Ok(result)
}

// Like Microsoft's PowerToys service, duplicate the target session's winlogon
// SYSTEM identity, preserving its logon groups and integrity/UIAccess attributes.
pub(super) fn session_token(session: u32) -> Result<Handle> {
    let mut pid = None;
    for process in desktop_processes()? {
        if process.name.eq_ignore_ascii_case("winlogon.exe")
            && process_session(process.pid)? == session
        {
            ensure!(pid.is_none(), "控制台会话存在多个 winlogon 进程");
            pid = Some(process.pid);
        }
    }
    let pid = pid.context("目标控制台会话的 winlogon 进程尚未就绪")?;
    let process = Handle::new(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) },
        "OpenProcess winlogon",
    )?;
    // Bind the selected PID to the Windows system binary, not merely its basename.
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    checked(
        unsafe { QueryFullProcessImageNameW(process.0, 0, path.as_mut_ptr(), &mut length) },
        "读取 winlogon 路径",
    )?;
    let path = String::from_utf16_lossy(&path[..length as usize]);
    let mut directory = vec![0u16; 32768];
    let length = unsafe { GetSystemDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
    ensure!(
        length > 0 && (length as usize) < directory.len(),
        "读取 Windows 系统目录失败: {}",
        std::io::Error::last_os_error()
    );
    let expected = format!(
        r"{}\winlogon.exe",
        String::from_utf16_lossy(&directory[..length as usize])
    );
    ensure!(
        path.eq_ignore_ascii_case(&expected),
        "winlogon 程序不在 Windows 系统目录"
    );
    let source = open_token(process.0, TOKEN_QUERY | TOKEN_DUPLICATE)?;
    ensure!(
        system_token(source.0)? && scalar(source.0, TokenSessionId)? == session,
        "winlogon 令牌身份或会话不匹配"
    );
    let mut token = null_mut();
    checked(
        unsafe {
            DuplicateTokenEx(
                source.0,
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_IMPERSONATE,
                null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut token,
            )
        },
        "复制控制台 winlogon 令牌",
    )?;
    let token = Handle::new(token, "Console token")?;
    super::service::log(&format!(
        "Console token source: winlogon pid={pid}, session={session}; {}",
        token_report(token.0).unwrap_or_else(|e| json!({"error":format!("{e:#}")}))
    ));
    Ok(token)
}

struct WtsMemory(*mut std::ffi::c_void);
impl Drop for WtsMemory {
    fn drop(&mut self) {
        unsafe { WTSFreeMemory(self.0) };
    }
}
fn protocol(session: u32) -> Result<u16> {
    let mut buffer = null_mut();
    let mut bytes = 0;
    checked(
        unsafe {
            WTSQuerySessionInformationW(
                null_mut(),
                session,
                WTSClientProtocolType,
                &mut buffer,
                &mut bytes,
            )
        },
        "WTSClientProtocolType",
    )?;
    let _memory = WtsMemory(buffer.cast());
    ensure!(
        !buffer.is_null() && bytes >= size_of::<u16>() as u32,
        "WTS 协议数据无效"
    );
    Ok(unsafe { *buffer })
}
fn sessions() -> Result<Value> {
    let mut buffer = null_mut();
    let mut count = 0;
    checked(
        unsafe { WTSEnumerateSessionsW(null_mut(), 0, 1, &mut buffer, &mut count) },
        "WTSEnumerateSessionsW",
    )?;
    let _memory = WtsMemory(buffer.cast());
    if count == 0 {
        return Ok(json!([]));
    }
    ensure!(!buffer.is_null(), "WTS 会话数据为空");
    let sessions = unsafe { std::slice::from_raw_parts(buffer, count as usize) };
    Ok(Value::Array(sessions.iter().map(|session| json!({"id":session.SessionId,"state":connection_state(session.State),"state_code":session.State,"protocol":protocol(session.SessionId).map_or_else(|e| json!({"error":format!("{e:#}")}), |v| json!(v))})).collect()))
}
fn connection_state(state: WTS_CONNECTSTATE_CLASS) -> &'static str {
    const STATES: [(WTS_CONNECTSTATE_CLASS, &str); 10] = [
        (WTSActive, "Active"),
        (WTSConnected, "Connected"),
        (WTSConnectQuery, "ConnectQuery"),
        (WTSShadow, "Shadow"),
        (WTSDisconnected, "Disconnected"),
        (WTSIdle, "Idle"),
        (WTSListen, "Listen"),
        (WTSReset, "Reset"),
        (WTSDown, "Down"),
        (WTSInit, "Init"),
    ];
    STATES
        .iter()
        .find(|(code, _)| *code == state)
        .map_or("Unknown", |(_, name)| *name)
}
pub(super) fn current_token_report() -> Result<Value> {
    token_report(open_token(unsafe { GetCurrentProcess() }, TOKEN_QUERY)?.0)
}
// Whitelist process metadata only: no command lines, usernames or window titles.
pub(super) fn report() -> Value {
    let processes = desktop_processes().map(|processes| Value::Array(processes.into_iter().map(|p| {
        let token = (|| {
            let process = Handle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, p.pid) }, "OpenProcess diagnostics")?;
            token_report(open_token(process.0, TOKEN_QUERY)?.0)
        })();
        json!({"pid":p.pid,"name":p.name,"session":process_session(p.pid).map_or_else(|e| json!({"error":format!("{e:#}")}), |v| json!(v)),"token":token.unwrap_or_else(|e:anyhow::Error| json!({"error":format!("{e:#}")}))})
    }).collect()));
    json!({"console_session":unsafe {WTSGetActiveConsoleSessionId()},"sessions":sessions().unwrap_or_else(|e|json!({"error":format!("{e:#}")})),"processes":processes.unwrap_or_else(|e|json!({"error":format!("{e:#}")}))})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interactive_diagnostics_read_current_token_and_sessions() -> Result<()> {
        let token = current_token_report()?;
        assert_eq!(
            token["session"],
            process_session(unsafe { GetCurrentProcessId() })?
        );
        assert!(token["system"].is_boolean());
        assert!(token["ui_access"].is_boolean());
        assert!(token["integrity_rid"].is_number());
        assert!(token["logon_sids"].is_array());
        let sessions = sessions()?;
        assert!(sessions.is_array());
        assert!(report()["processes"].is_array());
        Ok(())
    }
}
