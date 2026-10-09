mod protocol {
    pub use landesk::protocol::*;
}
#[path = "../src/platform/windows/desktop/wire.rs"]
mod wire;
use wire::{Action, Event, Request};
#[test]
fn control_characters_at_clipboard_limit_survive_ipc() {
    let text = "\u{0001}".repeat(protocol::MAX_TEXT_BYTES);
    let data = serde_json::to_vec(&Request {
        id: 7,
        action: Action::Input {
            message: protocol::ClientMessage::Text { text: text.clone() },
        },
    })
    .unwrap();
    let request = wire::decode_request(&data).unwrap();
    assert_eq!(request.id, 7);
    match request.action {
        Action::Input {
            message: protocol::ClientMessage::Text { text: value },
        } => assert_eq!(value, text),
        _ => panic!("unexpected command"),
    }
}
#[test]
fn malformed_or_oversized_commands_are_rejected() {
    assert!(
        wire::decode_request(b"{\"id\":1,\"action\":{\"type\":\"release\"},\"unknown\":true}")
            .is_err()
    );
    assert!(wire::decode_request(b"{\"id\":1,\"action\":{\"type\":\"exec\"}}").is_err());
    assert!(wire::decode_request(&vec![b' '; wire::MAX_COMMAND + 1]).is_err());
    assert!(wire::decode_request(b"{\"id\":1").is_err());
}
#[test]
fn consecutive_packets_keep_exact_boundaries() {
    let mut bytes = Vec::new();
    wire::event(
        &mut bytes,
        Event::Ready {
            version: wire::VERSION,
            width: 1920,
            height: 1080,
        },
    )
    .unwrap();
    wire::write_packet(&mut bytes, 1, &[255, 216, 1, 2, 255, 217]).unwrap();
    wire::event(
        &mut bytes,
        Event::Reply {
            id: 9,
            error: Some("切换桌面".into()),
        },
    )
    .unwrap();
    let mut offset = 0;
    let mut packets = Vec::new();
    while offset < bytes.len() {
        let size = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let tag = bytes[offset + 4];
        offset += 5;
        packets.push((tag, bytes[offset..offset + size].to_vec()));
        offset += size;
    }
    assert_eq!(packets.iter().map(|v| v.0).collect::<Vec<_>>(), [0, 1, 0]);
    assert!(matches!(
        serde_json::from_slice::<Event>(&packets[0].1).unwrap(),
        Event::Ready {
            width: 1920,
            height: 1080,
            ..
        }
    ));
    assert_eq!(packets[1].1, [255, 216, 1, 2, 255, 217]);
    assert!(matches!(
        serde_json::from_slice::<Event>(&packets[2].1).unwrap(),
        Event::Reply {
            id: 9,
            error: Some(_)
        }
    ));
}
#[test]
fn oversized_frame_does_not_write_a_partial_header() {
    let mut bytes = Vec::new();
    assert!(wire::write_packet(&mut bytes, 1, &vec![0; wire::MAX_PACKET + 1]).is_err());
    assert!(bytes.is_empty());
}

#[cfg(windows)]
#[path = "../src/platform/windows/desktop/worker_io.rs"]
mod worker_io;
#[cfg(windows)]
#[test]
fn windows_pipe_can_receive_input_while_sending_frames_and_stop_on_disconnect() {
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*, System::Pipes::*};
    let name = format!(r"\\.\pipe\LanDesk-test-{}", std::process::id());
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let pipe = unsafe {
        let handle = CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            65536,
            65536,
            0,
            std::ptr::null(),
        );
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        std::os::windows::io::OwnedHandle::from_raw_handle(handle)
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel(4);
    let task = runtime.block_on(async {
        let mut client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .unwrap();
        unsafe {
            assert_eq!(
                ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()),
                0
            );
            assert_eq!(GetLastError(), ERROR_PIPE_CONNECTED);
        }
        let mut output = worker_io::start(pipe, tx, stopped.clone()).unwrap();
        let task = std::thread::spawn(move || {
            wire::write_packet(&mut output, 1, &vec![42; 200_000]).unwrap();
            let command = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(command.id, 19);
            wire::event(
                &mut output,
                Event::Reply {
                    id: 19,
                    error: None,
                },
            )
            .unwrap();
        });
        let request = serde_json::to_vec(&Request {
            id: 19,
            action: Action::Release,
        })
        .unwrap();
        client.write_u32_le(request.len() as u32).await.unwrap();
        client.write_all(&request).await.unwrap();
        let read = async {
            assert_eq!(client.read_u32_le().await.unwrap(), 200_000);
            assert_eq!(client.read_u8().await.unwrap(), 1);
            let mut pixels = vec![0; 200_000];
            client.read_exact(&mut pixels).await.unwrap();
            assert!(pixels.iter().all(|v| *v == 42));
            let length = client.read_u32_le().await.unwrap();
            assert_eq!(client.read_u8().await.unwrap(), 0);
            let mut reply = vec![0; length as usize];
            client.read_exact(&mut reply).await.unwrap();
            assert!(matches!(
                serde_json::from_slice::<Event>(&reply).unwrap(),
                Event::Reply {
                    id: 19,
                    error: None
                }
            ));
        };
        tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .unwrap();
        drop(client);
        task
    });
    task.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stopped.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}
