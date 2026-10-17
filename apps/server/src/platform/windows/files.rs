//! User-scoped file access. SYSTEM only obtains the console user's token;
//! every path operation runs under that token on the dedicated file worker.
use super::desktop::{checked, wide};
use crate::adapters::file_session::{self, MAX_FILE_BYTES, PAGE_SIZE, TEMP_PREFIX};
use anyhow::{Context, Result, ensure};
use landesk_core::domain::{FileEntry, FileKind, FileResponse};
use landesk_core::protocol::UPLOAD_CHUNK_BYTES;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::mem::{offset_of, size_of};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::RemoteDesktop::*;
use windows_sys::Win32::UI::Shell::GetUserProfileDirectoryW;

pub struct UserFiles;
struct User {
    token: OwnedHandle,
    session: u32,
    logon: (u32, i32),
    root: PathBuf,
}
fn console_token(session: u32) -> Result<OwnedHandle> {
    ensure!(
        session != u32::MAX,
        "当前没有 Windows 控制台用户，请登录后重新连接"
    );
    let mut token = null_mut();
    checked(
        unsafe { WTSQueryUserToken(session, &mut token) },
        "获取当前登录用户身份（不会使用 SYSTEM 访问文件）",
    )?;
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}
fn logon_id(token: &OwnedHandle) -> Result<(u32, i32)> {
    let mut stats = TOKEN_STATISTICS::default();
    let mut length = 0;
    checked(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenStatistics,
                (&mut stats as *mut TOKEN_STATISTICS).cast(),
                size_of::<TOKEN_STATISTICS>() as u32,
                &mut length,
            )
        },
        "读取登录身份",
    )?;
    ensure!(
        length as usize == size_of::<TOKEN_STATISTICS>(),
        "登录身份长度无效"
    );
    Ok((
        stats.AuthenticationId.LowPart,
        stats.AuthenticationId.HighPart,
    ))
}
impl User {
    fn current() -> Result<Self> {
        super::service::require_system_console()?;
        // The worker duplicates winlogon's token, not the service host's token.
        // Enable WTSQueryUserToken's required privilege explicitly on this worker.
        super::service::enable_privileges(&["SeTcbPrivilege"])?;
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        let token = console_token(session)?;
        let logon = logon_id(&token)?;
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        checked(
            unsafe {
                GetUserProfileDirectoryW(token.as_raw_handle(), path.as_mut_ptr(), &mut length)
            },
            "读取登录用户目录",
        )?;
        ensure!(
            length > 1 && length as usize <= path.len() && path[length as usize - 1] == 0,
            "用户目录长度无效"
        );
        let root = PathBuf::from(String::from_utf16(&path[..length as usize - 1])?);
        ensure!(root.is_absolute(), "用户目录必须是绝对路径");
        Ok(Self {
            token,
            session,
            logon,
            root,
        })
    }
    fn enter(&self) -> Result<Impersonation> {
        ensure!(
            unsafe { WTSGetActiveConsoleSessionId() } == self.session,
            "Windows 登录会话已切换，请重新连接"
        );
        let current = console_token(self.session)?;
        ensure!(
            logon_id(&current)? == self.logon,
            "Windows 用户已重新登录，请重新连接"
        );
        Impersonation::enter(&self.token)
    }
}
// This guard cannot move to another thread and never spans an async await.
struct Impersonation(std::marker::PhantomData<std::rc::Rc<()>>);
impl Impersonation {
    fn enter(token: &OwnedHandle) -> Result<Self> {
        checked(
            unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) },
            "切换到登录用户身份失败，已拒绝文件操作",
        )?;
        Ok(Self(std::marker::PhantomData))
    }
}
impl Drop for Impersonation {
    fn drop(&mut self) {
        if unsafe { RevertToSelf() } == 0 {
            // Microsoft requires terminating if thread identity cannot be restored.
            std::process::abort();
        }
    }
}

fn valid_name(name: &str) -> Result<()> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    ensure!(
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.ends_with(['.', ' '])
            && !name.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c))
            && !name.to_ascii_lowercase().starts_with(TEMP_PREFIX)
            && !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            )
            && !(stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|s| matches!(
                    s,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                ))),
        "文件名包含非法、设备或保留名称"
    );
    ensure!(name.encode_utf16().count() <= 255, "文件名过长");
    Ok(())
}

/// Hold every directory in the path without write/delete sharing. This prevents
/// a checked ancestor from being renamed or replaced by a junction during IO.
struct Directory {
    path: PathBuf,
    _guards: Vec<File>,
}
impl Directory {
    fn open(root: &Path, relative: &str) -> Result<Self> {
        ensure!(root.is_absolute() && relative.len() <= 4096, "目录路径无效");
        let mut path = PathBuf::new();
        let mut guards = Vec::new();
        for part in root.components() {
            path.push(part.as_os_str());
            match part {
                Component::Prefix(_) => continue,
                Component::RootDir | Component::Normal(_) => guards.push(Self::lock(&path)?),
                _ => anyhow::bail!("用户根目录包含非法路径段"),
            }
        }
        if !relative.is_empty() {
            for part in relative.split('/') {
                valid_name(part)?;
                path.push(part);
                guards.push(Self::lock(&path)?);
            }
        }
        Ok(Self {
            path,
            _guards: guards,
        })
    }
    fn lock(path: &Path) -> Result<File> {
        let file = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .with_context(|| format!("打开目录 {}", path.display()))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "不允许访问目录链接或重解析点：{}",
            path.display()
        );
        Ok(file)
    }
    fn list(&self, id: u32, root: &Path, relative: &str, cursor: &str) -> Result<FileResponse> {
        let mut entries = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("目录包含非 Unicode 文件名"))?;
            if name.to_ascii_lowercase().starts_with(TEMP_PREFIX) {
                continue;
            }
            // DirEntry::metadata does not traverse Windows reparse points.
            let metadata = match entry.metadata() {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let kind = if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                FileKind::Symlink
            } else if metadata.is_dir() {
                FileKind::Directory
            } else if metadata.is_file() {
                FileKind::File
            } else {
                FileKind::Other
            };
            let key = format!(
                "{}/{}",
                if matches!(kind, FileKind::Directory) {
                    0
                } else {
                    1
                },
                name
            );
            if key.as_str() <= cursor {
                continue;
            }
            entries.insert(
                key,
                FileEntry {
                    name,
                    kind,
                    size: metadata.len(),
                },
            );
            if entries.len() > PAGE_SIZE + 1 {
                entries.pop_last();
            }
        }
        let more = entries.len() > PAGE_SIZE;
        if more {
            entries.pop_last();
        }
        let next_cursor = if more {
            entries.last_key_value().map(|(key, _)| key.clone())
        } else {
            None
        };
        Ok(FileResponse::Directory {
            id,
            root: root.to_string_lossy().replace('\\', "/"),
            path: relative.into(),
            entries: entries.into_values().collect(),
            next_cursor,
        })
    }
}

struct Upload {
    id: u32,
    file: File,
    _directory: Directory,
    target: PathBuf,
    size: u64,
    written: u64,
    live: bool,
}
impl Upload {
    fn start(root: &Path, id: u32, path: &str, name: &str, size: u64) -> Result<Self> {
        ensure!(size <= MAX_FILE_BYTES, "单个文件不能超过 512 MiB");
        valid_name(name)?;
        let directory = Directory::open(root, path)?;
        // std canonicalize returns the extended Windows path, including UNC
        // form where applicable; ancestors remain locked during resolution.
        let native_directory = std::fs::canonicalize(&directory.path)?;
        let target = native_directory.join(name);
        match std::fs::symlink_metadata(&target) {
            Ok(_) => anyhow::bail!("同名文件已存在，拒绝覆盖: {name}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("检查上传目标失败"),
        }
        let random = getrandom::u64().map_err(|e| anyhow::anyhow!("生成上传临时名称失败：{e}"))?;
        let temporary = native_directory.join(format!("{TEMP_PREFIX}{random:016x}"));
        let raw = unsafe {
            CreateFileW(
                wide(&temporary.to_string_lossy()).as_ptr(),
                GENERIC_WRITE | DELETE,
                0,
                null(),
                CREATE_NEW,
                FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        ensure!(
            raw != INVALID_HANDLE_VALUE,
            "创建上传文件失败：{}",
            std::io::Error::last_os_error()
        );
        let file = unsafe { File::from_raw_handle(raw) };
        Ok(Self {
            id,
            file,
            _directory: directory,
            target,
            size,
            written: 0,
            live: true,
        })
    }
    fn chunk(&mut self, data: &[u8]) -> Result<FileResponse> {
        ensure!(
            !data.is_empty()
                && data.len() <= UPLOAD_CHUNK_BYTES
                && self.written + data.len() as u64 <= self.size,
            "上传分块或文件大小无效"
        );
        self.file.write_all(data)?;
        self.written += data.len() as u64;
        Ok(FileResponse::UploadProgress {
            id: self.id,
            written: self.written,
        })
    }
    fn finish(&mut self, cancelled: &AtomicBool) -> Result<FileResponse> {
        ensure!(self.written == self.size, "上传尚未完整接收");
        self.file.sync_all()?;
        ensure!(!cancelled.load(Ordering::Acquire), "文件会话已取消");
        let target = wide(&self.target.to_string_lossy());
        let length = offset_of!(FILE_RENAME_INFO, FileName) + target.len() * 2;
        let mut storage = vec![
            0usize;
            length
                .max(size_of::<FILE_RENAME_INFO>())
                .div_ceil(size_of::<usize>())
        ];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            // Zeroed ReplaceIfExists = false: racing target creation also fails.
            (*info).FileNameLength = ((target.len() - 1) * 2) as u32;
            std::ptr::copy_nonoverlapping(
                target.as_ptr(),
                (*info).FileName.as_mut_ptr(),
                target.len(),
            );
            checked(
                SetFileInformationByHandle(
                    self.file.as_raw_handle(),
                    FileRenameInfo,
                    info.cast(),
                    length as u32,
                ),
                "完成上传失败（同名文件不覆盖）",
            )?;
        }
        self.live = false;
        Ok(FileResponse::UploadDone { id: self.id })
    }
    fn cleanup(&mut self) -> Result<()> {
        if self.live {
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            checked(
                unsafe {
                    SetFileInformationByHandle(
                        self.file.as_raw_handle(),
                        FileDispositionInfo,
                        (&info as *const FILE_DISPOSITION_INFO).cast(),
                        size_of::<FILE_DISPOSITION_INFO>() as u32,
                    )
                },
                "清理上传临时文件失败",
            )?;
            self.live = false;
        }
        Ok(())
    }
}
impl Drop for Upload {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            super::service::log(&format!("File upload cleanup: {error:#}"));
        }
    }
}
struct UserStore(Result<Arc<User>, String>);
impl UserStore {
    fn user(&self) -> Result<&Arc<User>> {
        self.0.as_ref().map_err(|e| anyhow::anyhow!("{e}"))
    }
}
struct UserUpload {
    user: Arc<User>,
    upload: Upload,
}
impl file_session::Upload for UserUpload {
    fn id(&self) -> u32 {
        self.upload.id
    }
    fn chunk(&mut self, data: &[u8]) -> Result<FileResponse> {
        let _user = self.user.enter()?;
        self.upload.chunk(data)
    }
    fn finish(&mut self, cancelled: &AtomicBool) -> Result<FileResponse> {
        let _user = self.user.enter()?;
        self.upload.finish(cancelled)
    }
    // Cleanup uses only the handle opened with user-granted DELETE access.
    fn cleanup(&mut self) -> Result<()> {
        self.upload.cleanup()
    }
}
impl file_session::Store for UserStore {
    fn list(&self, id: u32, path: &str, cursor: &str) -> Result<FileResponse> {
        let user = self.user()?;
        let _identity = user.enter()?;
        Directory::open(&user.root, path)?.list(id, &user.root, path, cursor)
    }
    fn start(
        &self,
        id: u32,
        path: &str,
        name: &str,
        size: u64,
    ) -> Result<Box<dyn file_session::Upload>> {
        let user = self.user()?;
        let _identity = user.enter()?;
        Ok(Box::new(UserUpload {
            user: user.clone(),
            upload: Upload::start(&user.root, id, path, name, size)?,
        }))
    }
}
impl landesk_core::ports::FileBackend for UserFiles {
    fn open_session(
        self: Arc<Self>,
        cancelled: Arc<AtomicBool>,
    ) -> Box<dyn landesk_core::ports::FileSession> {
        // This executes lazily on FileService's dedicated OS thread, not Tokio.
        let user = User::current().map(Arc::new).map_err(|e| format!("{e:#}"));
        Box::new(file_session::Session::with_cancellation(
            Arc::new(UserStore(user)),
            cancelled,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use landesk_core::domain::FileCommand;
    use serde_json::{Value, to_value};
    struct TestStore(PathBuf);
    impl file_session::Store for TestStore {
        fn list(&self, id: u32, path: &str, cursor: &str) -> Result<FileResponse> {
            Directory::open(&self.0, path)?.list(id, &self.0, path, cursor)
        }
        fn start(
            &self,
            id: u32,
            path: &str,
            name: &str,
            size: u64,
        ) -> Result<Box<dyn file_session::Upload>> {
            Ok(Box::new(Upload::start(&self.0, id, path, name, size)?))
        }
    }
    impl file_session::Upload for Upload {
        fn id(&self) -> u32 {
            self.id
        }
        fn chunk(&mut self, bytes: &[u8]) -> Result<FileResponse> {
            self.chunk(bytes)
        }
        fn finish(&mut self, flag: &AtomicBool) -> Result<FileResponse> {
            self.finish(flag)
        }
        fn cleanup(&mut self) -> Result<()> {
            self.cleanup()
        }
    }
    fn start(session: &mut file_session::Session, path: &str, name: &str, size: u64) -> Value {
        to_value(session.message(FileCommand::UploadStart {
            id: 1,
            path: path.into(),
            name: name.into(),
            size,
        }))
        .unwrap()
    }
    #[test]
    fn user_context_failure_rejects_files_without_system_fallback() {
        let mut session =
            file_session::Session::new(Arc::new(UserStore(Err("no logged-on user".into()))));
        for command in [
            FileCommand::ListDirectory {
                id: 1,
                path: "".into(),
                cursor: "".into(),
            },
            FileCommand::UploadStart {
                id: 1,
                path: "".into(),
                name: "x".into(),
                size: 0,
            },
        ] {
            let reply = to_value(session.message(command)).unwrap();
            assert_eq!(reply["type"], "file_error");
            assert!(
                reply["message"]
                    .as_str()
                    .unwrap()
                    .contains("no logged-on user")
            );
            assert_eq!(reply["upload_active"], false);
        }
    }
    #[test]
    fn windows_upload_roundtrip_cancellation_and_no_overwrite() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = Arc::new(TestStore(root.path().into()));
        let mut session = file_session::Session::new(store.clone());
        let bytes = [0, 255, 128, 13, 10];
        assert_eq!(
            start(&mut session, "", "中文.bin", 5)["type"],
            "upload_ready"
        );
        assert_eq!(to_value(session.chunk(&bytes))?["written"], 5);
        assert!(!root.path().join("中文.bin").exists());
        assert_eq!(
            to_value(session.message(FileCommand::UploadFinish { id: 1 }))?["type"],
            "upload_done"
        );
        assert_eq!(std::fs::read(root.path().join("中文.bin"))?, bytes);
        assert_eq!(start(&mut session, "", "中文.bin", 0)["type"], "file_error");
        assert_eq!(start(&mut session, "", "race", 1)["type"], "upload_ready");
        session.chunk(b"x");
        std::fs::write(root.path().join("race"), b"keep")?;
        assert_eq!(
            to_value(session.message(FileCommand::UploadFinish { id: 1 }))?["type"],
            "file_error"
        );
        assert_eq!(std::fs::read(root.path().join("race"))?, b"keep");
        for name in ["cancel", "partial", "oversize", "disconnect", "expired"] {
            let mut session = file_session::Session::new(store.clone());
            assert_eq!(start(&mut session, "", name, 2)["type"], "upload_ready");
            session.chunk(b"a");
            match name {
                "cancel" => {
                    assert_eq!(
                        to_value(session.message(FileCommand::UploadCancel { id: 1 }))?["type"],
                        "upload_cancelled"
                    );
                }
                "partial" => {
                    assert_eq!(
                        to_value(session.message(FileCommand::UploadFinish { id: 1 }))?["type"],
                        "file_error"
                    );
                }
                "oversize" => {
                    assert_eq!(to_value(session.chunk(b"bc"))?["type"], "file_error");
                }
                "expired" => {
                    session.cancelled.store(true, Ordering::Release);
                    assert_eq!(to_value(session.chunk(b"b"))?["type"], "file_error");
                }
                _ => {}
            }
            drop(session);
            assert_eq!(
                std::fs::read_dir(root.path())?.count(),
                2,
                "Leaked temporary file after {name}"
            );
        }
        assert_eq!(
            start(&mut session, "", "large", MAX_FILE_BYTES + 1)["type"],
            "file_error"
        );
        assert_eq!(start(&mut session, "", "empty", 0)["type"], "upload_ready");
        assert_eq!(
            to_value(session.message(FileCommand::UploadFinish { id: 1 }))?["type"],
            "upload_done"
        );
        Ok(())
    }
    #[test]
    fn windows_paths_reject_traversal_streams_device_names_and_lock_parents() -> Result<()> {
        let root = tempfile::tempdir()?;
        for path in [
            "..",
            "../a",
            "C:/Windows",
            "a\\b",
            "/Windows",
            "a//b",
            "a/../b",
            "a:stream",
            "a.",
            "a ",
            "CON",
            "nul.txt",
            "COM¹.log",
            ".LANDESK-UPLOAD-x",
        ] {
            assert!(Directory::open(root.path(), path).is_err(), "{path}");
        }
        std::fs::create_dir(root.path().join("工程"))?;
        let mut upload = Upload::start(root.path(), 1, "工程", "test", 1)?;
        assert!(std::fs::rename(root.path().join("工程"), root.path().join("moved")).is_err());
        upload.chunk(b"x")?;
        upload.finish(&AtomicBool::new(false))?;
        drop(upload);
        std::fs::rename(root.path().join("工程"), root.path().join("moved"))?;
        assert_eq!(std::fs::read(root.path().join("moved/test"))?, b"x");
        Ok(())
    }
    #[test]
    fn windows_large_directory_pagination_has_no_missing_entries() -> Result<()> {
        let root = tempfile::tempdir()?;
        for index in 0..1003 {
            std::fs::write(root.path().join(format!("item-{index:04}")), [])?;
        }
        let directory = Directory::open(root.path(), "")?;
        let mut names = std::collections::BTreeSet::new();
        let mut cursor = String::new();
        loop {
            let page = to_value(directory.list(1, root.path(), "", &cursor)?)?;
            let entries = page["entries"].as_array().unwrap();
            assert!(entries.len() <= PAGE_SIZE);
            for entry in entries {
                assert!(names.insert(entry["name"].as_str().unwrap().to_owned()));
            }
            match page["next_cursor"].as_str() {
                Some(next) => cursor = next.to_owned(),
                None => break,
            }
        }
        assert_eq!(names.len(), 1003);
        Ok(())
    }
    #[test]
    #[ignore = "Requires Windows administrator or developer mode to create directory symlinks"]
    fn native_setup_files_reject_directory_reparse_points() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::os::windows::fs::symlink_dir(outside.path(), root.path().join("escape"))?;
        assert!(Directory::open(root.path(), "escape").is_err());
        assert!(Upload::start(root.path(), 1, "escape", "test", 0).is_err());
        let page = to_value(Directory::open(root.path(), "")?.list(1, root.path(), "", "")?)?;
        assert_eq!(page["entries"][0]["kind"], "symlink");
        assert_eq!(std::fs::read_dir(outside.path())?.count(), 0);
        Ok(())
    }
    #[test]
    fn impersonation_is_thread_scoped_and_failure_is_explicit() -> Result<()> {
        use windows_sys::Win32::System::Threading::*;
        let mut token = null_mut();
        checked(
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_QUERY | TOKEN_DUPLICATE,
                    &mut token,
                )
            },
            "test token",
        )?;
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        {
            let _identity = Impersonation::enter(&token)?;
            let mut thread = null_mut();
            checked(
                unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut thread) },
                "test thread token",
            )?;
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            assert_eq!(logon_id(&thread)?, logon_id(&token)?);
        }
        let mut thread = null_mut();
        assert_eq!(
            unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut thread) },
            0
        );
        assert_eq!(unsafe { GetLastError() }, ERROR_NO_TOKEN);
        let event = unsafe { CreateEventW(null(), 0, 0, null()) };
        ensure!(!event.is_null(), "test event failed");
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        assert!(Impersonation::enter(&event).is_err());
        assert_eq!(
            unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut thread) },
            0
        );
        assert_eq!(unsafe { GetLastError() }, ERROR_NO_TOKEN);
        Ok(())
    }
}
