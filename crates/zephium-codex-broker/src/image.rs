//! Held exact-image admission. No PATH lookup or caller-selected digest.
use ::windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf, Prefix},
};

const REPARSE: u32 = 0x400;
const DIRECTORY: u32 = 0x10;
const OPEN_NOFOLLOW: u32 = 0x0020_0000;
const OPEN_DIRECTORY: u32 = 0x0200_0000;

/// Held files prevent replacement through ordinary Windows sharing semantics.
/// This is trusted fixed-image custody, not hostile same-user attestation.
pub(crate) struct PinnedImage {
    kind: crate::pin::ImageKind,
    path: PathBuf,
    _file: File,
    _ancestors: Vec<File>,
}
impl PinnedImage {
    pub(crate) fn open(path: &Path, kind: crate::pin::ImageKind) -> Result<Self, &'static str> {
        require_absolute_local(path)?;
        let mut ancestors = Vec::new();
        for ancestor in path
            .ancestors()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            ancestors.push(pin_directory(ancestor)?);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(OPEN_NOFOLLOW)
            .open(path)
            .map_err(|_| "image-open")?;
        let meta = file.metadata().map_err(|_| "image-metadata")?;
        if !meta.is_file()
            || meta.file_attributes() & (REPARSE | DIRECTORY) != 0
            || meta.len() != kind.bytes()
        {
            return Err("image-shape");
        }
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: file owns the valid handle and the output is a live local value.
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| "image-identity")?;
        if info.nNumberOfLinks != 1 {
            return Err("image-links");
        }
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|_| "image-read")?;
            if read == 0 {
                break;
            }
            bytes = bytes.checked_add(read as u64).ok_or("image-length")?;
            if bytes > kind.bytes() {
                return Err("image-length");
            }
            hash.update(&buffer[..read]);
        }
        if bytes != kind.bytes() || format!("{:x}", hash.finalize()) != kind.sha256() {
            return Err("image-hash");
        }
        Ok(Self {
            kind,
            path: path.to_owned(),
            _file: file,
            _ancestors: ancestors,
        })
    }
    pub(crate) fn kind(&self) -> crate::pin::ImageKind {
        self.kind
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn require_absolute_local(path: &Path) -> Result<(), &'static str> {
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || !matches!(parts.next(), Some(Component::RootDir))
        || parts.any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("path-shape");
    }
    Ok(())
}

pub(crate) fn pin_directory(path: &Path) -> Result<File, &'static str> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(OPEN_NOFOLLOW | OPEN_DIRECTORY)
        .open(path)
        .map_err(|_| "directory-open")?;
    let metadata = file.metadata().map_err(|_| "directory-metadata")?;
    if !metadata.is_dir() || metadata.file_attributes() & REPARSE != 0 {
        return Err("directory-shape");
    }
    Ok(file)
}
