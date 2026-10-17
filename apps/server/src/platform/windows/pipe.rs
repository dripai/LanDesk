//! Private desktop IPC. Verify the SYSTEM-owned pipe before sending credentials.
use super::desktop::checked;
use super::desktop::wide;
use super::service;
use crate::adapters::ssh::AUTH_HEADER;
use crate::adapters::web as server;
use crate::adapters::web::AppState;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use std::mem::size_of;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::task::JoinSet;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;

const NAME: &str = r"\\.\pipe\LanDeskServer.Desktop";
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
                wide("O:SYG:SYD:P(A;;FA;;;SY)(A;;GRGW;;;LS)(A;;GRGW;;;BA)").as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        },
        "创建内部管道权限",
    )?;
    let descriptor = Descriptor(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let pipe = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                NAME,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }?;
    Ok(pipe)
}
fn verify_owner(pipe: &NamedPipeClient) -> Result<()> {
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    // .NET PipeSecurity likewise treats named pipes as KernelObject security.
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
    ensure!(error == ERROR_SUCCESS, "无法验证内部管道身份 ({error})");
    let _descriptor = Descriptor(descriptor);
    ensure!(
        !owner.is_null() && unsafe { IsWellKnownSid(owner, WinLocalSystemSid) } != 0,
        "内部管道不是 SYSTEM 所有，已拒绝连接"
    );
    Ok(())
}
pub async fn connect() -> Result<NamedPipeClient> {
    let pipe = loop {
        // Tokio uses SECURITY_IDENTIFICATION, preventing a server impersonating us.
        match ClientOptions::new().open(NAME) {
            Ok(pipe) => break pipe,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(error) => return Err(error).context("桌面工作进程未就绪"),
        }
    };
    verify_owner(&pipe)?;
    Ok(pipe)
}
pub async fn serve(state: AppState) -> Result<()> {
    let shared = state.shared.clone();
    let router = server::router(state);
    let mut next = create(true)?;
    let mut connections = JoinSet::new();
    service::log("Desktop HTTP ready on protected named pipe");
    loop {
        tokio::select! {
            result = next.connect() => {
                result.context("内部管道连接失败")?;
                let pipe = std::mem::replace(&mut next, create(false)?);
                let router = router.clone();
                connections.spawn(async move {
                    hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(pipe), TowerToHyperService::new(router)).with_upgrades().await
                });
            }
            result = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = result { return Err(error).context("内部管道任务异常"); }
                // A closed browser/probe closes only its own HTTP connection.
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if shared.shutdown.load(Ordering::Acquire) { return Ok(()); }
            }
        }
    }
}
pub async fn health(key: &str) -> Result<()> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut pipe = connect().await?;
        pipe.write_all(format!("GET / HTTP/1.1\r\nHost: landesk.internal\r\n{AUTH_HEADER}: {key}\r\nConnection: close\r\n\r\n").as_bytes()).await?;
        let mut response = String::new();
        pipe.take(65536).read_to_string(&mut response).await?;
        ensure!(response.starts_with("HTTP/1.1 200") && response.contains("id=\"connect-form\""), "桌面 HTTP 检查失败");
        Ok::<_, anyhow::Error>(())
    }).await.context("桌面 HTTP 检查超时")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn untrusted_pipe_owner_is_rejected_before_any_credential_is_sent() {
        let name = format!(r"\\.\pipe\LanDeskOwnerTest-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        // Standard developer/CI tokens are not SYSTEM. No elevation or real service needed.
        assert!(verify_owner(&client).is_err());
    }
}
