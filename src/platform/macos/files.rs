use crate::protocol::UPLOAD_CHUNK_BYTES;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::Write,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const TEMP_PREFIX: &str = ".landesk-upload-";
const MAX_ENTRIES: usize = 5000;

pub struct HomeFiles {
    root: File,
    label: String,
}

fn name(value: &str) -> Result<CString> {
    ensure!(
        !value.is_empty() && value != "." && value != "..",
        "文件名无效"
    );
    ensure!(
        !value.contains('/') && !value.contains('\0'),
        "文件名包含非法字符"
    );
    ensure!(!value.starts_with(TEMP_PREFIX), "文件名为上传保留名称");
    Ok(CString::new(value)?)
}

fn open_directory(parent: &File, component: &CStr) -> Result<File> {
    // Each component is opened relative to an already verified directory handle.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "无法打开目录（不允许跟随符号链接）: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl HomeFiles {
    pub fn open(path: &Path) -> Result<Self> {
        let label = path.to_str().context("用户目录不是 UTF-8 路径")?.to_owned();
        let path = CString::new(label.as_bytes())?;
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(
            fd >= 0,
            "无法打开用户目录: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            root: unsafe { File::from_raw_fd(fd) },
            label,
        })
    }

    fn directory(&self, path: &str) -> Result<File> {
        ensure!(path.len() <= 4096, "目录路径过长");
        let mut directory = self.root.try_clone()?;
        if !path.is_empty() {
            for component in path.split('/') {
                directory = open_directory(&directory, &name(component)?)?;
            }
        }
        Ok(directory)
    }

    fn list(&self, id: u32, path: &str) -> Result<Value> {
        let directory = self.directory(path)?;
        // fdopendir owns this descriptor; reopen '.' so concurrent listings do not share offsets.
        let scan = open_directory(&directory, c".")?;
        let fd = scan.into_raw_fd();
        let pointer = unsafe { libc::fdopendir(fd) };
        if pointer.is_null() {
            unsafe {
                libc::close(fd);
            }
            bail!("无法读取目录: {}", std::io::Error::last_os_error());
        }
        struct DirectoryStream(*mut libc::DIR);
        impl Drop for DirectoryStream {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let stream = DirectoryStream(pointer);
        let mut entries = Vec::new();
        loop {
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                ensure!(
                    unsafe { *libc::__error() } == 0,
                    "读取目录失败: {}",
                    std::io::Error::last_os_error()
                );
                break;
            }
            let raw_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let filename = raw_name.to_str().context("目录包含非 UTF-8 文件名")?;
            if filename == "." || filename == ".." || filename.starts_with(TEMP_PREFIX) {
                continue;
            }
            ensure!(
                entries.len() < MAX_ENTRIES,
                "目录条目超过 5000 个，请使用更具体的目录"
            );
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            ensure!(
                unsafe {
                    libc::fstatat(
                        directory.as_raw_fd(),
                        raw_name.as_ptr(),
                        &mut stat,
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } == 0,
                "无法读取文件信息 {filename}: {}",
                std::io::Error::last_os_error()
            );
            let kind = match stat.st_mode & libc::S_IFMT {
                libc::S_IFDIR => "directory",
                libc::S_IFREG => "file",
                libc::S_IFLNK => "symlink",
                _ => "other",
            };
            entries.push(json!({"name":filename,"kind":kind,"size":stat.st_size.max(0)}));
        }
        entries.sort_by(|a, b| {
            (a["kind"] != "directory")
                .cmp(&(b["kind"] != "directory"))
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        });
        Ok(json!({"type":"directory","id":id,"root":self.label,"path":path,"entries":entries}))
    }
}

struct Upload {
    id: u32,
    directory: File,
    file: File,
    temporary: CString,
    target: CString,
    expected: u64,
    written: u64,
    live: bool,
}

impl Upload {
    fn start(home: &HomeFiles, id: u32, path: &str, filename: &str, size: u64) -> Result<Self> {
        ensure!(size <= MAX_FILE_BYTES, "单个文件不能超过 512 MiB");
        let target = name(filename)?;
        let directory = home.directory(path)?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let exists = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                target.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if exists == 0 {
            bail!("同名文件已存在，拒绝覆盖: {filename}");
        }
        ensure!(
            std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
            "无法检查目标文件: {}",
            std::io::Error::last_os_error()
        );
        let random = getrandom::u64().map_err(|e| anyhow::anyhow!("无法生成上传临时名称: {e}"))?;
        let temporary = CString::new(format!("{TEMP_PREFIX}{random:016x}"))?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        ensure!(
            fd >= 0,
            "无法创建上传文件: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            id,
            directory,
            file: unsafe { File::from_raw_fd(fd) },
            temporary,
            target,
            expected: size,
            written: 0,
            live: true,
        })
    }

    fn chunk(&mut self, data: &[u8]) -> Result<Value> {
        ensure!(
            !data.is_empty() && data.len() <= UPLOAD_CHUNK_BYTES,
            "上传分块必须为 1–64 KiB"
        );
        ensure!(
            self.written + data.len() as u64 <= self.expected,
            "上传数据超过声明的文件大小"
        );
        self.file.write_all(data).context("写入上传文件失败")?;
        self.written += data.len() as u64;
        Ok(json!({"type":"upload_progress","id":self.id,"written":self.written}))
    }

    fn finish(&mut self, cancelled: &AtomicBool) -> Result<Value> {
        ensure!(self.written == self.expected, "上传尚未完整接收");
        self.file.sync_all().context("保存上传文件失败")?;
        ensure!(!cancelled.load(Ordering::Acquire), "文件会话已取消");
        // macOS 10.12+: one atomic rename, with no replacement even if the target appeared during upload.
        ensure!(
            unsafe {
                libc::renameatx_np(
                    self.directory.as_raw_fd(),
                    self.temporary.as_ptr(),
                    self.directory.as_raw_fd(),
                    self.target.as_ptr(),
                    libc::RENAME_EXCL,
                )
            } == 0,
            "无法完成上传（同名文件不覆盖）: {}",
            std::io::Error::last_os_error()
        );
        self.live = false;
        Ok(json!({"type":"upload_done","id":self.id}))
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.live {
            ensure!(
                unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.temporary.as_ptr(), 0) }
                    == 0,
                "清理上传临时文件失败: {}",
                std::io::Error::last_os_error()
            );
            self.live = false;
        }
        Ok(())
    }
}
impl Drop for Upload {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("{error:#}");
        }
    }
}

impl crate::platform::FileSystem for HomeFiles {
    fn list(&self, id: u32, path: &str) -> Result<Value> {
        self.list(id, path)
    }
    fn start_upload(
        &self,
        id: u32,
        path: &str,
        name: &str,
        size: u64,
    ) -> Result<Box<dyn crate::platform::FileTransfer>> {
        Ok(Box::new(Upload::start(self, id, path, name, size)?))
    }
}
impl crate::platform::FileTransfer for Upload {
    fn id(&self) -> u32 {
        self.id
    }
    fn chunk(&mut self, data: &[u8]) -> Result<Value> {
        self.chunk(data)
    }
    fn finish(&mut self, cancelled: &AtomicBool) -> Result<Value> {
        self.finish(cancelled)
    }
    fn cleanup(&mut self) -> Result<()> {
        self.cleanup()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileSession;
    use crate::protocol::ClientMessage;
    use std::sync::Arc;
    use std::{fs, os::unix::fs::symlink, path::PathBuf};

    struct Fixture {
        path: PathBuf,
        home: Arc<HomeFiles>,
    }
    impl Fixture {
        fn new() -> Self {
            let path = PathBuf::from(format!(
                "/private/tmp/landesk-files-test-{:016x}",
                getrandom::u64().unwrap()
            ));
            fs::create_dir(&path).unwrap();
            let home = Arc::new(HomeFiles::open(&path).unwrap());
            Self { path, home }
        }
        fn session(&self) -> FileSession {
            FileSession::new(self.home.clone())
        }
        fn start(&self, session: &mut FileSession, filename: &str, size: u64) -> Value {
            session.message(ClientMessage::UploadStart {
                id: 1,
                path: String::new(),
                name: filename.into(),
                size,
            })
        }
        fn count(&self) -> usize {
            fs::read_dir(&self.path).unwrap().count()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }

    #[test]
    fn cancelled_upload_is_not_published_and_cleans_temporary_file() {
        let f = Fixture::new();
        let mut session = f.session();
        assert_eq!(
            f.start(&mut session, "cancelled.txt", 0)["type"],
            "upload_ready"
        );
        session.cancel();
        let response = session.message(ClientMessage::UploadFinish { id: 1 });
        assert_eq!(response["type"], "file_error");
        assert_eq!(response["upload_active"], false);
        assert_eq!(f.count(), 0);
    }

    #[test]
    fn list_is_repeatable_and_links_are_not_directories() {
        let f = Fixture::new();
        fs::create_dir(f.path.join("工程")).unwrap();
        fs::write(f.path.join("hello.rs"), "你好").unwrap();
        symlink("/", f.path.join("outside")).unwrap();
        let a = f.home.list(1, "").unwrap();
        let b = f.home.list(1, "").unwrap();
        assert_eq!(a, b);
        assert_eq!(a["entries"][0]["kind"], "directory");
        assert!(
            a["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["name"] == "outside" && entry["kind"] == "symlink")
        );
        assert!(f.home.directory("outside").is_err());
    }

    #[test]
    fn traversal_absolute_paths_and_invalid_names_are_rejected() {
        let f = Fixture::new();
        for path in ["..", "/Users/example", "a/../b", "a//b", "./a", "a\0b"] {
            assert!(f.home.directory(path).is_err(), "{path}");
        }
        for filename in ["", ".", "..", "../a", "/a", "a\0b", ".landesk-upload-x"] {
            assert_eq!(f.start(&mut f.session(), filename, 0)["type"], "file_error");
        }
        assert_eq!(f.count(), 0);
    }

    #[test]
    fn complete_upload_preserves_binary_bytes_and_publishes_only_at_finish() {
        let f = Fixture::new();
        let mut session = f.session();
        let bytes = vec![0, 255, 13, 10, 128];
        assert_eq!(
            f.start(&mut session, "内容.bin", bytes.len() as u64)["type"],
            "upload_ready"
        );
        assert_eq!(session.chunk(&bytes)["type"], "upload_progress");
        assert!(!f.path.join("内容.bin").exists());
        assert!(
            f.home.list(1, "").unwrap()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            session.message(ClientMessage::UploadFinish { id: 1 })["type"],
            "upload_done"
        );
        assert_eq!(fs::read(f.path.join("内容.bin")).unwrap(), bytes);
        assert_eq!(f.count(), 1);
    }

    #[test]
    fn same_name_and_target_created_during_upload_never_get_overwritten() {
        let f = Fixture::new();
        let mut session = f.session();
        fs::write(f.path.join("exists"), "original").unwrap();
        assert_eq!(f.start(&mut session, "exists", 0)["type"], "file_error");
        assert_eq!(f.start(&mut session, "race", 3)["type"], "upload_ready");
        session.chunk(b"new");
        fs::write(f.path.join("race"), "original").unwrap();
        assert_eq!(
            session.message(ClientMessage::UploadFinish { id: 1 })["type"],
            "file_error"
        );
        assert_eq!(fs::read(f.path.join("race")).unwrap(), b"original");
        assert_eq!(fs::read(f.path.join("exists")).unwrap(), b"original");
        assert_eq!(f.count(), 2);
    }

    #[test]
    fn interrupted_cancelled_and_incomplete_uploads_remove_temporary_files() {
        let f = Fixture::new();
        {
            let mut session = f.session();
            f.start(&mut session, "a", 3);
            session.chunk(b"x");
        }
        assert_eq!(f.count(), 0);
        let mut session = f.session();
        f.start(&mut session, "b", 3);
        assert_eq!(
            session.message(ClientMessage::UploadCancel { id: 1 })["type"],
            "upload_cancelled"
        );
        assert_eq!(f.count(), 0);
        f.start(&mut session, "c", 3);
        session.chunk(b"x");
        let result = session.message(ClientMessage::UploadFinish { id: 1 });
        assert_eq!(result["type"], "file_error");
        assert_eq!(result["upload_active"], false);
        assert_eq!(f.count(), 0);
    }

    #[test]
    fn empty_files_work_and_size_chunk_limits_clean_up() {
        let f = Fixture::new();
        let mut session = f.session();
        f.start(&mut session, "empty", 0);
        assert_eq!(
            session.message(ClientMessage::UploadFinish { id: 1 })["type"],
            "upload_done"
        );
        assert_eq!(fs::metadata(f.path.join("empty")).unwrap().len(), 0);
        assert_eq!(
            f.start(&mut session, "huge", MAX_FILE_BYTES + 1)["type"],
            "file_error"
        );
        f.start(&mut session, "overrun", 1);
        assert_eq!(session.chunk(b"xx")["type"], "file_error");
        f.start(&mut session, "large-chunk", MAX_FILE_BYTES);
        assert_eq!(
            session.chunk(&vec![0; UPLOAD_CHUNK_BYTES + 1])["type"],
            "file_error"
        );
        assert_eq!(f.count(), 1);
    }

    #[test]
    fn uploads_cannot_follow_links_and_keep_the_open_parent_handle() {
        let f = Fixture::new();
        let outside = Fixture::new();
        let mut session = f.session();
        symlink(&outside.path, f.path.join("escape")).unwrap();
        let start = |path: &str| ClientMessage::UploadStart {
            id: 1,
            path: path.into(),
            name: "test".into(),
            size: 1,
        };
        assert_eq!(session.message(start("escape"))["type"], "file_error");
        fs::create_dir(f.path.join("work")).unwrap();
        assert_eq!(session.message(start("work"))["type"], "upload_ready");
        fs::rename(f.path.join("work"), f.path.join("moved")).unwrap();
        symlink(&outside.path, f.path.join("work")).unwrap();
        session.chunk(b"x");
        assert_eq!(
            session.message(ClientMessage::UploadFinish { id: 1 })["type"],
            "upload_done"
        );
        assert_eq!(fs::read(f.path.join("moved/test")).unwrap(), b"x");
        assert_eq!(outside.count(), 0);
    }
}
