//! Operations and results understood by native backends.
use serde::Serialize;

#[derive(Debug)]
pub enum InputEvent {
    Pointer { x: f64, y: f64 },
    Button { button: u8, down: bool },
    Wheel { x: i32, y: i32 },
    Key { key: String, down: bool },
    Text { text: String },
    ReleaseAll,
}

#[derive(Debug)]
pub enum FileCommand {
    ListDirectory {
        id: u32,
        path: String,
        cursor: String,
    },
    UploadStart {
        id: u32,
        path: String,
        name: String,
        size: u64,
    },
    UploadFinish {
        id: u32,
    },
    UploadCancel {
        id: u32,
    },
}
impl FileCommand {
    pub fn id(&self) -> u32 {
        match self {
            Self::ListDirectory { id, .. }
            | Self::UploadStart { id, .. }
            | Self::UploadFinish { id }
            | Self::UploadCancel { id } => *id,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileResponse {
    Directory {
        id: u32,
        root: String,
        path: String,
        entries: Vec<FileEntry>,
        next_cursor: Option<String>,
    },
    UploadReady {
        id: u32,
        chunk_size: usize,
    },
    UploadProgress {
        id: u32,
        written: u64,
    },
    UploadDone {
        id: u32,
    },
    UploadCancelled {
        id: u32,
    },
    FileError {
        id: u32,
        message: String,
        upload_active: bool,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub main: bool,
    #[serde(rename = "virtual")]
    pub virtual_display: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CaptureInfo {
    pub width: u32,
    pub height: u32,
    pub native_width: u32,
    pub native_height: u32,
    pub requested_width: Option<u32>,
    pub display_id: u32,
    pub displays: Vec<DisplayInfo>,
}
