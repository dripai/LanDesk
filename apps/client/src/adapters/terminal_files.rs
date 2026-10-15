//! File operations use their own SFTP channel, never shell commands or PTY bytes.
use crate::adapters::terminal::Socket;
use crate::adapters::terminal::send_json;
use crate::adapters::tunnel::Handler;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use futures_util::StreamExt;
use russh::ChannelMsg;
use russh::client;
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::FileAttributes;
use russh_sftp::protocol::OpenFlags;
use russh_sftp::protocol::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Hello {
        token: String,
    },
    ListDirectory {
        id: u64,
        path: String,
        cursor: String,
    },
    UploadStart {
        id: u64,
        path: String,
        name: String,
        size: u64,
    },
    UploadFinish {
        id: u64,
    },
    UploadCancel {
        id: u64,
    },
}
struct Upload {
    id: u64,
    handle: Option<String>,
    temporary: String,
    destination: String,
    size: u64,
    written: u64,
}
struct Directory {
    handle: String,
    path: String,
    cursor: String,
    pending: VecDeque<Value>,
    eof: bool,
}
struct Files {
    sftp: RawSftpSession,
    upload: Option<Upload>,
    directory: Option<Directory>,
    sequence: u64,
}

fn path(value: &str) -> Result<String> {
    ensure!(
        value.len() <= 4096 && !value.contains('\0'),
        "无效的目录路径"
    );
    ensure!(
        value.is_empty()
            || value
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."),
        "无效的目录路径"
    );
    Ok(format!("/{value}"))
}
fn filename(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 255
            && !value.contains(['/', '\0'])
            && value != "."
            && value != "..",
        "无效的文件名"
    );
    Ok(())
}
fn entry_kind(permissions: Option<u32>) -> &'static str {
    match permissions.map(|mode| mode & 0o170000) {
        Some(0o040000) => "directory",
        Some(0o100000) => "file",
        Some(0o120000) => "symlink",
        _ => "other",
    }
}
fn status_is(error: &SftpError, code: StatusCode) -> bool {
    matches!(error, SftpError::Status(status) if status.status_code == code)
}
impl Files {
    async fn close_directory(&mut self) -> Result<()> {
        if let Some(dir) = self.directory.take() {
            self.sftp.close(dir.handle).await?;
        }
        Ok(())
    }
    async fn cancel(&mut self) -> Result<()> {
        if let Some(upload) = self.upload.take() {
            let closed = if let Some(handle) = upload.handle {
                self.sftp.close(handle).await.map(|_| ())
            } else {
                Ok(())
            };
            let removed = self.sftp.remove(upload.temporary.clone()).await;
            if let Err(error) = removed {
                bail!("未完成上传的清理失败：{}：{error}", upload.temporary);
            }
            closed?;
        }
        Ok(())
    }
    async fn list(&mut self, id: u64, relative: String, cursor: String) -> Result<Value> {
        let absolute = path(&relative)?;
        if cursor.is_empty() {
            self.close_directory().await?;
            let handle = self.sftp.opendir(absolute).await?.handle;
            self.directory = Some(Directory {
                handle,
                path: relative.clone(),
                cursor: String::new(),
                pending: VecDeque::new(),
                eof: false,
            });
        } else {
            let dir = self.directory.as_ref().context("目录分页已失效，请刷新")?;
            ensure!(
                dir.path == relative && dir.cursor == cursor,
                "目录分页已失效，请刷新"
            );
        }
        let dir = self.directory.as_mut().context("缺少目录句柄")?;
        while dir.pending.len() <= 500 && !dir.eof {
            match self.sftp.readdir(dir.handle.clone()).await {
                Ok(names) => {
                    ensure!(!names.files.is_empty(), "SFTP 返回空目录批次但未结束");
                    for file in names.files {
                        if file.filename == "." || file.filename == ".." {
                            continue;
                        }
                        filename(&file.filename)?;
                        dir.pending.push_back(json!({"name":file.filename,"kind":entry_kind(file.attrs.permissions),"size":file.attrs.size.unwrap_or(0)}));
                    }
                }
                Err(error) if status_is(&error, StatusCode::Eof) => dir.eof = true,
                Err(error) => return Err(error.into()),
            }
        }
        let count = dir.pending.len().min(500);
        let mut entries: Vec<_> = dir.pending.drain(..count).collect();
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let next = if dir.pending.is_empty() && dir.eof {
            self.close_directory().await?;
            None
        } else {
            self.sequence += 1;
            let cursor = self.sequence.to_string();
            self.directory.as_mut().unwrap().cursor = cursor.clone();
            Some(cursor)
        };
        Ok(
            json!({"type":"directory","id":id,"root":"/","path":relative,"entries":entries,"next_cursor":next}),
        )
    }
    async fn start(&mut self, id: u64, relative: String, name: String, size: u64) -> Result<Value> {
        ensure!(self.upload.is_none(), "已有上传正在进行");
        ensure!(size <= 512 * 1024 * 1024, "文件超过 512 MiB");
        filename(&name)?;
        let directory = path(&relative)?;
        let destination = format!("{}/{name}", directory.trim_end_matches('/'));
        match self.sftp.lstat(destination.clone()).await {
            Ok(_) => bail!("同名文件已存在，不会覆盖"),
            Err(error) if status_is(&error, StatusCode::NoSuchFile) => {}
            Err(error) => return Err(error.into()),
        }
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("无法生成上传文件名：{e}"))?;
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let temporary = format!("{}/.landesk-{suffix}.part", directory.trim_end_matches('/'));
        let handle = self
            .sftp
            .open(
                temporary.clone(),
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..Default::default()
                },
            )
            .await?
            .handle;
        self.upload = Some(Upload {
            id,
            handle: Some(handle),
            temporary,
            destination,
            size,
            written: 0,
        });
        Ok(json!({"type":"upload_ready","id":id}))
    }
    async fn chunk(&mut self, bytes: &[u8]) -> Result<Value> {
        let upload = self.upload.as_mut().context("没有进行中的上传")?;
        ensure!(
            !bytes.is_empty()
                && bytes.len() <= 64 * 1024
                && upload.written + bytes.len() as u64 <= upload.size,
            "上传数据长度不匹配"
        );
        for part in bytes.chunks(32 * 1024) {
            self.sftp
                .write(
                    upload.handle.as_ref().context("上传已关闭")?.clone(),
                    upload.written,
                    part.to_vec(),
                )
                .await?;
            upload.written += part.len() as u64;
        }
        Ok(json!({"type":"upload_progress","id":upload.id,"written":upload.written}))
    }
    async fn finish(&mut self, id: u64) -> Result<Value> {
        let upload = self.upload.as_mut().context("没有进行中的上传")?;
        ensure!(
            upload.id == id && upload.written == upload.size,
            "上传标识或文件长度不匹配"
        );
        if let Some(handle) = upload.handle.take() {
            self.sftp.close(handle).await?;
        }
        // SFTP v3 RENAME rejects an existing destination. Do not use posix-rename.
        self.sftp
            .rename(upload.temporary.clone(), upload.destination.clone())
            .await?;
        self.upload = None;
        Ok(json!({"type":"upload_done","id":id}))
    }
}

pub(crate) async fn run(
    socket: &mut Socket,
    ssh: Arc<client::Handle<Handler>>,
    activity: watch::Sender<landesk_core::application::viewer::Activity>,
    token: &str,
) -> Result<()> {
    let message = timeout(Duration::from_secs(10), socket.next())
        .await?
        .context("文件连接已关闭")??;
    let Message::Text(text) = message else {
        bail!("缺少文件握手")
    };
    let Request::Hello { token: supplied } = serde_json::from_str(&text)? else {
        bail!("缺少文件握手")
    };
    ensure!(supplied == token, "文件会话凭据已失效，请刷新页面");
    let _viewer = landesk_core::application::viewer::Viewer::new(activity);
    let mut channel = timeout(Duration::from_secs(10), ssh.channel_open_session())
        .await
        .context("SFTP 通道创建超时")??;
    let setup = timeout(Duration::from_secs(10), async {
        channel.request_subsystem(true, "sftp").await?;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => return Ok::<_, anyhow::Error>(()),
                Some(ChannelMsg::WindowAdjusted { .. }) => {}
                _ => bail!("服务器拒绝 SFTP 文件通道，请检查 SSH 的 SFTP 配置"),
            }
        }
    })
    .await
    .context("SFTP 子系统初始化超时")
    .and_then(|result| result);
    if let Err(error) = setup {
        timeout(Duration::from_secs(3), channel.close())
            .await
            .context("关闭失败的 SFTP 通道超时")??;
        return Err(error);
    }
    let sftp = RawSftpSession::new(channel.into_stream());
    sftp.set_timeout(10);
    ensure!(sftp.init().await?.version == 3, "只支持 SFTP v3");
    let mut files = Files {
        sftp,
        upload: None,
        directory: None,
        sequence: 0,
    };
    let result = async {
        send_json(socket,json!({"type":"files_ready"})).await?;
        while let Some(message) = socket.next().await {
            let mut id = files.upload.as_ref().map_or(0, |u| u.id);
            let operation = match message? {
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Binary(bytes) => files.chunk(&bytes).await,
                Message::Text(text) => match serde_json::from_str::<Request>(&text)? {
                    Request::ListDirectory {id: request, path, cursor} => { id = request; files.list(id,path,cursor).await },
                    Request::UploadStart {id: request,path,name,size} => {id=request;files.start(id,path,name,size).await},
                    Request::UploadFinish {id: request} => {id=request;files.finish(id).await},
                    Request::UploadCancel {id: request} => {
                        id=request;
                        if files.upload.as_ref().is_some_and(|u| u.id != id) { Err(anyhow::anyhow!("上传标识不匹配")) }
                        else { files.cancel().await.map(|_|json!({"type":"upload_cancelled","id":id})) }
                    },
                    Request::Hello {..} => bail!("不能重复文件握手"),
                },
                _ => bail!("无效的文件消息"),
            };
            match operation {
                Ok(reply) => send_json(socket,reply).await?,
                Err(error) => {
                    send_json(socket,json!({"type":"file_error","id":id,"message":format!("{error:#}"),"upload_active":files.upload.is_some()})).await?;
                    if error.downcast_ref::<SftpError>().is_some_and(|e| !matches!(e, SftpError::Status(_))) { return Err(error); }
                }
            }
        }
        Ok(())
    }.await;
    let upload_cleanup = files.cancel().await;
    let directory_cleanup = files.close_directory().await;
    let closed = files.sftp.close_session();
    upload_cleanup.context("文件连接结束时清理上传失败")?;
    directory_cleanup?;
    closed?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn special_files_and_links_are_not_classified_by_overlapping_mode_bits() {
        assert_eq!(entry_kind(Some(0o120777)), "symlink");
        assert_eq!(entry_kind(Some(0o140777)), "other");
        assert_eq!(entry_kind(Some(0o060600)), "other");
        assert_eq!(entry_kind(Some(0o040755)), "directory");
        assert_eq!(entry_kind(Some(0o100644)), "file");
        assert_eq!(entry_kind(None), "other");
    }
    #[test]
    fn remote_paths_are_posix_on_every_client_os() {
        assert_eq!(path("etc/nginx").unwrap(), "/etc/nginx");
        assert_eq!(path("").unwrap(), "/");
        for invalid in ["../etc", "a/../b", "/etc", "a//b", "a\0b"] {
            assert!(path(invalid).is_err());
        }
        for invalid in ["", "..", "a/b", "a\0b"] {
            assert!(filename(invalid).is_err());
        }
        assert!(filename("中文 ' $ 文件.txt").is_ok());
    }
}
