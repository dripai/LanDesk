use super::{
    service,
    sys::*,
    wire::{self, Action, Event, Request},
};
use crate::{platform::FrameEvent, protocol::ClientMessage};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    os::windows::io::{AsRawHandle, IntoRawHandle},
    ptr,
    sync::{Arc, Mutex, Weak, mpsc},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::NamedPipeClient,
    sync::{mpsc as async_mpsc, watch},
};
use windows_sys::Win32::{Storage::FileSystem::*, System::Pipes::*};
static CURRENT: Mutex<Weak<Broker>> = Mutex::new(Weak::new());
type Started = (Arc<Broker>, u32, u32, watch::Receiver<Option<FrameEvent>>);
type Reply = mpsc::SyncSender<std::result::Result<(), String>>;
struct Command {
    action: Action,
    reply: Reply,
}
pub struct Broker {
    commands: async_mpsc::Sender<Command>,
    stop: watch::Sender<bool>,
}
impl Broker {
    pub fn current() -> Result<Arc<Self>> {
        CURRENT
            .lock()
            .unwrap()
            .upgrade()
            .context("桌面采集连接尚未建立")
    }
    pub fn start() -> Result<Started> {
        let mut current = CURRENT.lock().unwrap();
        ensure!(current.upgrade().is_none(), "上次桌面连接仍在关闭");
        let (commands, rx) = async_mpsc::channel(64);
        let (stop, stopped) = watch::channel(false);
        let (frames, receiver) = watch::channel(None);
        let (ready, result) = mpsc::sync_channel(1);
        let broker = Arc::new(Self { commands, stop });
        std::thread::Builder::new()
            .name("landesk-desktop-broker".into())
            .spawn(move || {
                let outcome = (|| -> Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(connection(rx, stopped, &frames, &ready))
                })();
                if let Err(error) = outcome {
                    let message = format!("{error:#}");
                    let _ = ready.try_send(Err(message.clone()));
                    let _ = frames.send(Some(FrameEvent::Error(message)));
                }
            })?;
        let (width, height) = result
            .recv_timeout(Duration::from_secs(15))
            .context("等待桌面服务响应失败")?
            .map_err(anyhow::Error::msg)?;
        *current = Arc::downgrade(&broker);
        Ok((broker, width, height, receiver))
    }
    pub fn call(&self, action: Action) -> Result<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command { action, reply })
            .map_err(|_| anyhow::anyhow!("桌面服务命令通道不可用"))?;
        result
            .recv_timeout(Duration::from_secs(3))
            .context("桌面服务未响应操作")?
            .map_err(anyhow::Error::msg)
    }
    pub fn close(&self) {
        let _ = self.stop.send(true);
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.close();
    }
}
fn connect() -> Result<NamedPipeClient> {
    let expected = service::process_id()?;
    unsafe {
        let handle = owned(
            CreateFileW(
                wide(service::PIPE).as_ptr(),
                service::PIPE_ACCESS,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            ),
            "连接 LanDesk 后台服务失败",
        )?;
        let mut pid = 0;
        ok(
            GetNamedPipeServerProcessId(handle.as_raw_handle(), &mut pid),
            "验证后台服务失败",
        )?;
        ensure!(pid == expected, "桌面通道不属于 LanDesk 后台服务");
        Ok(NamedPipeClient::from_raw_handle(handle.into_raw_handle())?)
    }
}
async fn connection(
    mut commands: async_mpsc::Receiver<Command>,
    mut stopped: watch::Receiver<bool>,
    frames: &watch::Sender<Option<FrameEvent>>,
    ready: &mpsc::SyncSender<std::result::Result<(u32, u32), String>>,
) -> Result<()> {
    let pipe = connect()?;
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let replies = Mutex::new(HashMap::<u64, Reply>::new());
    let reading = async {
        let mut initialized = false;
        let mut sequence = 0;
        loop {
            let length = reader.read_u32_le().await? as usize;
            ensure!(length <= wire::MAX_PACKET, "后台服务数据包过大");
            let tag = reader.read_u8().await?;
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).await?;
            match tag {
                0 => match serde_json::from_slice::<Event>(&bytes)? {
                    Event::Ready {
                        version,
                        width,
                        height,
                    } => {
                        ensure!(
                            !initialized && version == wire::VERSION,
                            "后台服务协议不匹配"
                        );
                        super::worker::dimensions(width, height, 0)?;
                        initialized = true;
                        let _ = ready.send(Ok((width, height)));
                    }
                    Event::Reply { id, error } => {
                        if let Some(reply) = replies.lock().unwrap().remove(&id) {
                            let _ = reply.send(error.map_or(Ok(()), Err));
                        }
                    }
                    Event::Error { message } => anyhow::bail!("{message}"),
                },
                1 => {
                    ensure!(initialized, "后台服务尚未初始化");
                    sequence += 1;
                    let _ = frames.send(Some(FrameEvent::Frame {
                        jpeg: Arc::new(bytes),
                        sequence,
                    }));
                }
                _ => anyhow::bail!("后台服务数据类型无效"),
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let writing = async {
        let mut id = 0;
        while let Some(command) = commands.recv().await {
            id += 1;
            let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&Request {
                id,
                action: command.action,
            })?);
            ensure!(bytes.len() <= wire::MAX_COMMAND, "桌面命令过大");
            replies.lock().unwrap().insert(id, command.reply);
            writer.write_u32_le(bytes.len() as u32).await?;
            writer.write_all(&bytes).await?;
            writer.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! { result=reading=>result,result=writing=>result,_=stopped.changed()=>Ok(()) }
}
pub struct Input(pub Arc<Broker>);
impl crate::platform::InputController for Input {
    fn handle(&mut self, message: ClientMessage) -> Result<()> {
        self.0.call(Action::Input { message })
    }
    fn paste(&mut self) -> Result<()> {
        self.0.call(Action::Paste)
    }
    fn release_all(&mut self) -> Result<()> {
        self.0.call(Action::Release)
    }
}
