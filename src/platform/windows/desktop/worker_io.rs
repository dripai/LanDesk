use super::wire::{self, Request};
use anyhow::{Result, ensure};
use std::{
    io::{self, Write},
    os::windows::io::{IntoRawHandle, OwnedHandle},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::NamedPipeServer,
    sync::mpsc as async_mpsc,
};
struct Packet {
    bytes: Vec<u8>,
    reply: mpsc::SyncSender<std::result::Result<(), String>>,
}
pub struct Output {
    sender: async_mpsc::Sender<Packet>,
    buffer: Vec<u8>,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.buffer.len() + bytes.len() > wire::MAX_PACKET + 5 {
            return Err(io::Error::other("桌面数据包过大"));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let (reply, result) = mpsc::sync_channel(1);
        let bytes = std::mem::take(&mut self.buffer);
        self.sender
            .blocking_send(Packet { bytes, reply })
            .map_err(|_| io::Error::other("桌面连接已关闭"))?;
        result
            .recv()
            .map_err(|_| io::Error::other("桌面连接已关闭"))?
            .map_err(io::Error::other)
    }
}
pub fn start(
    pipe: OwnedHandle,
    requests: mpsc::SyncSender<Request>,
    stopped: Arc<AtomicBool>,
) -> Result<Output> {
    let (sender, mut packets) = async_mpsc::channel::<Packet>(2);
    std::thread::Builder::new()
        .name("landesk-desktop-io".into())
        .spawn(move || {
            let _ = (|| -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let pipe = unsafe { NamedPipeServer::from_raw_handle(pipe.into_raw_handle())? };
                    let (mut reader, mut writer) = tokio::io::split(pipe);
                    let reading = async {
                        loop {
                            let length = reader.read_u32_le().await? as usize;
                            ensure!(length <= wire::MAX_COMMAND, "桌面命令过大");
                            let mut bytes = zeroize::Zeroizing::new(vec![0; length]);
                            reader.read_exact(&mut bytes).await?;
                            requests
                                .try_send(wire::decode_request(&bytes)?)
                                .map_err(|_| anyhow::anyhow!("桌面命令队列不可用"))?;
                        }
                        #[allow(unreachable_code)]
                        Ok::<(), anyhow::Error>(())
                    };
                    let writing = async {
                        while let Some(packet) = packets.recv().await {
                            let result = async {
                                writer.write_all(&packet.bytes).await?;
                                writer.flush().await
                            }
                            .await;
                            let error = result.as_ref().err().map(ToString::to_string);
                            let _ = packet.reply.send(error.map_or(Ok(()), Err));
                            result?;
                        }
                        Ok::<(), anyhow::Error>(())
                    };
                    tokio::select! {result=reading=>result,result=writing=>result}
                })
            })();
            stopped.store(true, Ordering::Release);
        })?;
    Ok(Output {
        sender,
        buffer: Vec::new(),
    })
}
