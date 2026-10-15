#![cfg(target_os = "macos")]
use landesk::platform::macos::files::HomeFiles;
use landesk_core::{application::file_worker::*, domain::FileCommand};
use std::time::Duration;
#[tokio::test]
async fn disconnect_cleans_partial_upload_before_next_file_session() {
    let root = std::path::PathBuf::from(format!(
        "/private/tmp/landesk-worker-test-{:016x}",
        getrandom::u64().unwrap()
    ));
    std::fs::create_dir(&root).unwrap();
    let service = FileService::new(HomeFiles::open(&root).unwrap());
    let mut files = service.connect();
    files.submit(FileRequest::Message(FileCommand::UploadStart {
        id: 1,
        path: String::new(),
        name: "partial.txt".into(),
        size: 10,
    }));
    assert_eq!(
        serde_json::to_value(files.response().await).unwrap()["type"],
        "upload_ready"
    );
    files.submit(FileRequest::Chunk(vec![1, 2]));
    assert_eq!(
        serde_json::to_value(files.response().await).unwrap()["written"],
        2
    );
    drop(files);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let mut next = service.connect();
            if next
                .submit(FileRequest::Message(FileCommand::ListDirectory {
                    id: 2,
                    path: String::new(),
                    cursor: String::new(),
                }))
                .is_none()
            {
                next.response().await;
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir(&root).unwrap();
}
