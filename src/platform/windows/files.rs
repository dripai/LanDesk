use crate::{
    platform::{FileSystem, FileTransfer},
    protocol::UPLOAD_CHUNK_BYTES,
};
use anyhow::{Context, Result, bail, ensure};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, File, OpenOptions, OpenOptionsExt};
use serde_json::{Value, json};
use std::{
    io::Write,
    os::windows::io::AsRawHandle,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::{
    Foundation::GENERIC_WRITE,
    Storage::FileSystem::{
        DELETE, FILE_RENAME_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FileRenameInfo,
        GetFinalPathNameByHandleW, SetFileInformationByHandle,
    },
};
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const PREFIX: &str = ".landesk-upload-";

fn destination_name(dir: &Dir, name: &str) -> Result<Vec<u16>> {
    // cap-std keeps this directory open without FILE_SHARE_DELETE. Resolve the
    // pinned directory's full path, not a path supplied by the browser.
    let handle = dir.as_raw_handle();
    let size = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    ensure!(
        size > 0,
        "无法解析上传目录：{}",
        std::io::Error::last_os_error()
    );
    let mut path = vec![0u16; size as usize];
    let length = unsafe { GetFinalPathNameByHandleW(handle, path.as_mut_ptr(), size, 0) };
    ensure!(
        length > 0 && length < size,
        "无法解析上传目录：{}",
        std::io::Error::last_os_error()
    );
    path.truncate(length as usize);
    if path.last() != Some(&(b'\\' as u16)) {
        path.push(b'\\' as u16);
    }
    path.extend(name.encode_utf16());
    Ok(path)
}

fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.ends_with(['.', ' '])
            && !name.starts_with(PREFIX),
        "文件名无效"
    );
    ensure!(
        !name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c)),
        "文件名包含非法字符"
    );
    let stem = name.split('.').next().unwrap().to_ascii_uppercase();
    let numbered_device = (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem[3..]
            .parse::<u8>()
            .ok()
            .is_some_and(|n| (1..=9).contains(&n));
    ensure!(
        !["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
            && !numbered_device,
        "Windows 保留文件名不可用"
    );
    Ok(())
}
pub struct HomeFiles {
    root: Dir,
    label: String,
}
impl HomeFiles {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            root: Dir::open_ambient_dir(path, cap_std::ambient_authority())?,
            label: path
                .to_str()
                .context("用户目录不是 UTF-8")?
                .replace('\\', "/"),
        })
    }
    fn directory(&self, path: &str) -> Result<Dir> {
        ensure!(path.len() <= 4096, "目录路径过长");
        let mut dir = self.root.try_clone()?;
        if !path.is_empty() {
            for part in path.split('/') {
                valid_name(part)?;
                dir = dir.open_dir_nofollow(part)?;
            }
        }
        Ok(dir)
    }
}
impl FileSystem for HomeFiles {
    fn list(&self, id: u32, path: &str) -> Result<Value> {
        let dir = self.directory(path)?;
        let mut entries = Vec::new();
        for entry in dir.entries()? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("目录包含非 UTF-8 文件名"))?;
            if name.starts_with(PREFIX) {
                continue;
            }
            ensure!(entries.len() < 5000, "目录条目超过 5000 个");
            let meta = dir.symlink_metadata(&name)?;
            let kind = if meta.is_symlink() {
                "symlink"
            } else if meta.is_dir() {
                "directory"
            } else if meta.is_file() {
                "file"
            } else {
                "other"
            };
            entries.push(json!({"name":name,"kind":kind,"size":meta.len()}));
        }
        entries.sort_by(|a, b| {
            (a["kind"] != "directory")
                .cmp(&(b["kind"] != "directory"))
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        });
        Ok(json!({"type":"directory","id":id,"root":self.label,"path":path,"entries":entries}))
    }
    fn start_upload(
        &self,
        id: u32,
        path: &str,
        name: &str,
        size: u64,
    ) -> Result<Box<dyn FileTransfer>> {
        valid_name(name)?;
        ensure!(size <= MAX_FILE_BYTES, "单个文件不能超过 512 MiB");
        let dir = self.directory(path)?;
        match dir.symlink_metadata(name) {
            Ok(_) => bail!("同名文件已存在，拒绝覆盖"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let temporary = format!(
            "{PREFIX}{:016x}",
            getrandom::u64().map_err(|e| anyhow::anyhow!(e))?
        );
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No)
            .access_mode(GENERIC_WRITE | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        let file = dir.open_with(&temporary, &options)?;
        Ok(Box::new(Upload {
            id,
            dir,
            file: Some(file),
            temporary,
            target: name.into(),
            expected: size,
            written: 0,
            live: true,
        }))
    }
}
struct Upload {
    id: u32,
    dir: Dir,
    file: Option<File>,
    temporary: String,
    target: String,
    expected: u64,
    written: u64,
    live: bool,
}
impl FileTransfer for Upload {
    fn id(&self) -> u32 {
        self.id
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
        self.file.as_mut().context("上传已结束")?.write_all(data)?;
        self.written += data.len() as u64;
        Ok(json!({"type":"upload_progress","id":self.id,"written":self.written}))
    }
    fn finish(&mut self, cancelled: &AtomicBool) -> Result<Value> {
        ensure!(self.written == self.expected, "上传尚未完整接收");
        let file = self.file.as_ref().context("上传已结束")?;
        file.sync_all()?;
        ensure!(!cancelled.load(Ordering::Acquire), "文件会话已取消");
        // Win32 FileRenameInfo uses an absolute destination and null root.
        // Keep the directory pinned and ReplaceIfExists=false during publish.
        let name = destination_name(&self.dir, &self.target)?;
        let bytes = (std::mem::offset_of!(FILE_RENAME_INFO, FileName) + (name.len() + 1) * 2)
            .max(std::mem::size_of::<FILE_RENAME_INFO>());
        let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        unsafe {
            let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).RootDirectory = std::ptr::null_mut();
            (*info).FileNameLength = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
            ensure!(
                SetFileInformationByHandle(
                    file.as_raw_handle(),
                    FileRenameInfo,
                    info.cast(),
                    bytes as u32
                ) != 0,
                "无法完成上传（同名文件不覆盖）：{}",
                std::io::Error::last_os_error()
            );
        }
        self.live = false;
        self.file.take();
        Ok(json!({"type":"upload_done","id":self.id}))
    }
    fn cleanup(&mut self) -> Result<()> {
        if self.live {
            self.file.take();
            self.dir.remove_file(&self.temporary)?;
            self.live = false;
        }
        Ok(())
    }
}
impl Drop for Upload {
    fn drop(&mut self) {
        if let Err(e) = self.cleanup() {
            eprintln!("清理上传失败：{e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_traversal_streams_and_device_names() {
        for name in [
            "..",
            "a/b",
            "a\\b",
            "x:secret",
            "CON",
            "nul.txt",
            "COM1",
            "trailing.",
        ] {
            assert!(valid_name(name).is_err(), "{name}");
        }
    }
    #[test]
    fn upload_publishes_atomically_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let home = HomeFiles::open(root.path()).unwrap();
        let mut upload = home.start_upload(1, "", "hello.txt", 3).unwrap();
        upload.chunk(b"abc").unwrap();
        assert!(!root.path().join("hello.txt").exists());
        upload.finish(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("hello.txt")).unwrap(),
            b"abc"
        );
        assert!(home.start_upload(2, "", "hello.txt", 0).is_err());
        let mut race = home.start_upload(3, "", "race", 0).unwrap();
        std::fs::write(root.path().join("race"), b"keep").unwrap();
        assert!(race.finish(&AtomicBool::new(false)).is_err());
        drop(race);
        assert_eq!(std::fs::read(root.path().join("race")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }
    #[test]
    fn upload_uses_pinned_unicode_directory_and_short_destination_name() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("目录");
        std::fs::create_dir(&nested).unwrap();
        let home = HomeFiles::open(root.path()).unwrap();
        let mut upload = home.start_upload(1, "目录", "文", 3).unwrap();
        assert!(std::fs::rename(&nested, root.path().join("moved")).is_err());
        upload.chunk(b"abc").unwrap();
        upload.finish(&AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read(nested.join("文")).unwrap(), b"abc");
        assert_eq!(std::fs::read_dir(nested).unwrap().count(), 1);
    }
}
