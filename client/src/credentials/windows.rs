use super::*;
use windows_sys::Win32::{Foundation::ERROR_NOT_FOUND, Security::Credentials::*};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

struct Credential(*mut CREDENTIALW);
impl Drop for Credential {
    fn drop(&mut self) {
        unsafe {
            let record = &mut *self.0;
            if record.CredentialBlobSize != 0 && !record.CredentialBlob.is_null() {
                std::slice::from_raw_parts_mut(
                    record.CredentialBlob,
                    record.CredentialBlobSize as usize,
                )
                .zeroize();
            }
            CredFree(self.0.cast());
        }
    }
}

pub(super) fn read_at(target: &str) -> Result<Option<SavedPassword>> {
    let target = wide(target);
    let mut pointer = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(None);
        }
        return Err(error).context("无法读取 Windows 凭据管理器");
    }
    ensure!(!pointer.is_null(), "Windows 返回了空凭据");
    let credential = Credential(pointer);
    let record = unsafe { &*credential.0 };
    ensure!(
        record.CredentialBlobSize != 0 && !record.CredentialBlob.is_null(),
        "保存的 SSH 凭据为空"
    );
    let bytes = unsafe {
        std::slice::from_raw_parts(record.CredentialBlob, record.CredentialBlobSize as usize)
    };
    Ok(Some(
        serde_json::from_slice(bytes).context("保存的 SSH 凭据损坏")?,
    ))
}

pub(super) fn write_at(target: &str, saved: Option<&SavedPassword>) -> Result<()> {
    let mut target_wide = wide(target);
    let Some(saved) = saved else {
        if unsafe { CredDeleteW(target_wide.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                return Err(error).context("无法删除保存的 SSH 密码");
            }
        }
        ensure!(
            read_at(target)?.is_none(),
            "Windows SSH 凭据删除后状态核验失败"
        );
        return Ok(());
    };
    let mut bytes = Zeroizing::new(serde_json::to_vec(saved)?);
    ensure!(
        bytes.len() <= CRED_MAX_CREDENTIAL_BLOB_SIZE as usize,
        "SSH 凭据超过 Windows 保存上限"
    );
    let mut username = wide(&saved.user);
    let record = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target_wide.as_mut_ptr(),
        CredentialBlobSize: bytes.len() as u32,
        CredentialBlob: bytes.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username.as_mut_ptr(),
        ..Default::default()
    };
    ensure!(
        unsafe { CredWriteW(&record, 0) } != 0,
        "无法保存 Windows SSH 凭据：{}",
        std::io::Error::last_os_error()
    );
    ensure!(
        read_at(target)?.as_ref() == Some(saved),
        "Windows SSH 凭据保存后状态核验失败"
    );
    Ok(())
}
