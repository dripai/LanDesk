use crate::settings::Settings;
use anyhow::{Context, Result, bail, ensure};
use russh::{
    Disconnect, client,
    keys::{self, PublicKeyOrCertificate},
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet, time::timeout};
use zeroize::Zeroizing;

pub const VIEWER_URL: &str = "http://127.0.0.1:17890";
const PORT: u16 = 17890;

pub enum Event {
    Status(String),
    Trust {
        fingerprint: String,
        reply: oneshot::Sender<bool>,
    },
    Ready,
    Stopped(Result<()>),
}

struct Handler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    events: mpsc::Sender<Event>,
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, server: &PublicKeyOrCertificate) -> Result<bool> {
        ensure!(
            server.certificate().is_none(),
            "首版暂不支持 SSH 主机证书，请使用普通主机密钥"
        );
        let key = server.public_key();
        if keys::check_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)
            .context("SSH 主机密钥已变化或信任文件无法读取，请核对 Mac 主机")?
        {
            return Ok(true);
        }
        let (reply, answer) = oneshot::channel();
        self.events.send(Event::Trust {
            fingerprint: key.fingerprint(keys::HashAlg::Sha256).to_string(),
            reply,
        })?;
        ensure!(
            timeout(Duration::from_secs(120), answer)
                .await
                .context("等待主机指纹确认超时")??,
            "未信任 SSH 主机，已取消连接"
        );
        keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)
            .context("无法保存已确认的 SSH 主机密钥")?;
        Ok(true)
    }
}

pub async fn run(
    settings: Settings,
    password: Zeroizing<String>,
    known_hosts: PathBuf,
    events: mpsc::Sender<Event>,
    mut cancel: oneshot::Receiver<()>,
) -> Result<()> {
    settings.validate()?;
    ensure!(!password.is_empty(), "请输入 Mac SSH 密码");
    // Reserve the fixed browser origin before authenticating. Never use another port silently.
    let listener = TcpListener::bind(("127.0.0.1", PORT))
        .await
        .context("本机 17890 端口被占用，请关闭旧连接脚本或另一个客户端")?;
    events.send(Event::Status("正在连接 SSH…".into()))?;
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 2,
        ..Default::default()
    });
    let handler = Handler {
        host: settings.host.clone(),
        port: settings.ssh_port,
        known_hosts,
        events: events.clone(),
    };
    let mut session = tokio::select! {
        _ = &mut cancel => return Ok(()),
        result = timeout(Duration::from_secs(140), client::connect(config, (settings.host.as_str(), settings.ssh_port), handler)) => result.context("SSH 连接超时")??,
    };
    let result = tokio::select! {
        _ = &mut cancel => Ok(()),
        result = async {
            events.send(Event::Status("正在验证 Mac 用户…".into()))?;
            let auth = timeout(Duration::from_secs(20), session.authenticate_password(&settings.user, password.to_string())).await.context("SSH 密码验证超时")??;
            drop(password);
            ensure!(auth.success(), "SSH 密码验证失败，请检查用户名、密码及 Mac 远程登录设置");
            let probe = timeout(Duration::from_secs(10), session.channel_open_direct_tcpip("127.0.0.1", PORT.into(), "127.0.0.1", 0)).await.context("Mac 服务检查超时")?.context("无法访问 Mac LanDesk，请先启动 Mac 应用")?;
            probe.close().await?;
            events.send(Event::Ready)?;
            let mut transfers = JoinSet::new();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = tick.tick() => { ensure!(!session.is_closed(), "SSH 连接已断开"); }
                    result = transfers.join_next(), if !transfers.is_empty() => {
                        if let Some(Err(error)) = result { bail!("隧道任务异常：{error}"); }
                        // Browser cancellation/reset is normal and affects only that TCP stream.
                    }
                    accepted = listener.accept(), if transfers.len() < 32 => {
                        let (mut socket, address) = accepted?;
                        let channel = timeout(Duration::from_secs(10), session.channel_open_direct_tcpip("127.0.0.1", PORT.into(), address.ip().to_string(), address.port().into())).await.context("转发通道打开超时")??;
                        transfers.spawn(async move { tokio::io::copy_bidirectional(&mut socket, &mut channel.into_stream()).await });
                    }
                }
            }
        } => result,
    };
    // Dropping the transfer JoinSet cancels every local socket before SSH is closed.
    let _ = timeout(
        Duration::from_secs(3),
        session.disconnect(Disconnect::ByApplication, "LanDeskClient disconnected", ""),
    )
    .await;
    result
}
