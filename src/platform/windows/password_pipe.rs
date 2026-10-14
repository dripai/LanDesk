//! Local interactive users may view the connection password. This pipe returns
//! only its encrypted record, never the transport's host key or backend key.
use super::desktop::{checked, wide};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    mem::size_of, os::windows::io::AsRawHandle, path::PathBuf, ptr::null_mut, time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
};

const NAME: &str = r"\\.\pipe\LanDeskServer.PasswordView";
const MAX_RECORD: u64 = 16384;
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn create(first: bool) -> Result<NamedPipeServer> {
    let mut descriptor = null_mut();
    checked(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide("O:LSG:LSD:P(A;;FA;;;SY)(A;;FA;;;LS)(A;;GRGW;;;BA)(A;;GRGW;;;IU)").as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        },
        "创建密码界面管道权限",
    )?;
    let descriptor = Descriptor(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    Ok(unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                NAME,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }?)
}
fn verify_owner(pipe: &NamedPipeClient) -> Result<()> {
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    let error = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    ensure!(error == ERROR_SUCCESS, "无法验证密码界面管道身份 ({error})");
    let _descriptor = Descriptor(descriptor);
    ensure!(
        !owner.is_null() && unsafe { IsWellKnownSid(owner, WinLocalServiceSid) } != 0,
        "密码界面管道不属于 LanDesk 后台服务"
    );
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordInfo {
    pub port: u16,
    secret: crate::secret_storage::Secret,
}
impl PasswordInfo {
    pub fn password(&self) -> Result<zeroize::Zeroizing<String>> {
        let bytes = self.secret.decrypt(crate::transport::PASSWORD_PURPOSE)?;
        let value =
            zeroize::Zeroizing::new(String::from_utf8(bytes.to_vec()).context("连接密码编码无效")?);
        crate::connection_password::validate(&value)?;
        Ok(value)
    }
}
pub struct Listener(NamedPipeServer);
impl Listener {
    pub fn new() -> Result<Self> {
        Ok(Self(create(true)?))
    }
    pub async fn serve(mut self, path: PathBuf) -> Result<()> {
        loop {
            self.0.connect().await.context("密码界面管道连接失败")?;
            let mut pipe = std::mem::replace(&mut self.0, create(false)?);
            let path = path.clone();
            let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                let config = crate::transport::Config::read(&path)?;
                let _ = config.recover_password()?;
                Ok(serde_json::to_vec(&PasswordInfo {
                    port: config.port,
                    secret: config.password_secret(),
                })?)
            })
            .await??;
            // A closed or stalled UI does not stop remote control.
            let _ = tokio::time::timeout(Duration::from_secs(2), pipe.write_all(&data)).await;
        }
    }
}
pub async fn read() -> Result<PasswordInfo> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let pipe = loop {
            match ClientOptions::new().open(NAME) {
                Ok(pipe) => break pipe,
                Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => return Err(error).context("连接密码界面服务失败"),
            }
        };
        verify_owner(&pipe)?;
        let mut data = Vec::new();
        pipe.take(MAX_RECORD + 1).read_to_end(&mut data).await?;
        ensure!(data.len() as u64 <= MAX_RECORD, "密码界面响应过大");
        serde_json::from_slice(&data).context("密码界面响应无效")
    })
    .await
    .context("读取连接密码超时，请检查后台服务日志")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn password_view_rejects_pipe_not_owned_by_local_service() {
        let name = format!(r"\\.\pipe\LanDeskPasswordOwnerTest-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        assert!(verify_owner(&client).is_err());
    }
}
