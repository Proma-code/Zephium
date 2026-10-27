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
#[cfg(all(debug_assertions, feature = "native-fixtures"))]
const DIRECTORY: u32 = 0x10;
const OPEN_NOFOLLOW: u32 = 0x0020_0000;
const OPEN_DIRECTORY: u32 = 0x0200_0000;

/// Held files prevent replacement through ordinary Windows sharing semantics.
/// This is trusted fixed-image custody, not hostile same-user attestation.
pub(crate) struct PinnedImage {
    provenance: Provenance,
    kind: crate::pin::ImageKind,
    path: PathBuf,
    _file: File,
    _ancestors: Vec<File>,
}
enum Provenance {
    Reference(crate::pin::ReviewedCli),
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    StandaloneReference,
    Official(crate::release_admission::OriginalCliArtifact),
}
impl PinnedImage {
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
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
            provenance: if kind == crate::pin::ImageKind::Cli {
                Provenance::Reference(crate::pin::ReviewedCli::V164)
            } else {
                Provenance::StandaloneReference
            },
            kind,
            path: path.to_owned(),
            _file: file,
            _ancestors: ancestors,
        })
    }
    pub(crate) fn projection(&self) -> crate::compatibility::ConfigProjection {
        match &self.provenance {
            Provenance::Reference(crate::pin::ReviewedCli::V163) => {
                crate::compatibility::ConfigProjection::V163
            }
            Provenance::Reference(_) => crate::compatibility::ConfigProjection::V164Plus,
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            Provenance::StandaloneReference => crate::compatibility::ConfigProjection::V164Plus,
            Provenance::Official(artifact) => {
                crate::compatibility::ConfigProjection::for_artifact(artifact)
            }
        }
    }
    pub(crate) fn version(&self) -> &str {
        match &self.provenance {
            Provenance::Reference(pin) => pin.version(),
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            Provenance::StandaloneReference => crate::RELEASE,
            Provenance::Official(artifact) => artifact.version(),
        }
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

pub(crate) mod resolution;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExistingImageFault {
    Resolve(resolution::ResolveFault),
    Unsupported,
    Cancelled,
    Timeout,
}
/// Non-executable discovery result. Only verify() can mint PinnedImage.
pub(crate) struct CliCandidate {
    held: resolution::ResolvedFile,
}
impl CliCandidate {
    pub(crate) fn open(path: &Path) -> Result<Self, ExistingImageFault> {
        let held = resolution::local_file(path).map_err(ExistingImageFault::Resolve)?;
        let metadata = held
            .file
            .metadata()
            .map_err(|_| ExistingImageFault::Resolve(resolution::ResolveFault::Unavailable))?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(held.file.as_raw_handle()), &mut info) }
            .map_err(|_| ExistingImageFault::Resolve(resolution::ResolveFault::Unavailable))?;
        if metadata.len() == 0 || metadata.len() > 1024 * 1024 * 1024 || info.nNumberOfLinks != 1 {
            return Err(ExistingImageFault::Unsupported);
        }
        Ok(Self { held })
    }
    pub(crate) fn identity(&self) -> (u64, [u8; 16]) {
        self.held.identity
    }
    pub(crate) fn hash(
        mut self,
        deadline: std::time::Instant,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<HashedCli, ExistingImageFault> {
        use std::sync::atomic::Ordering;
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(ExistingImageFault::Cancelled)
            } else if std::time::Instant::now() >= deadline {
                Err(ExistingImageFault::Timeout)
            } else {
                Ok(())
            }
        };
        let expected = self
            .held
            .file
            .metadata()
            .map_err(|_| ExistingImageFault::Unsupported)?
            .len();
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            check()?;
            let length =
                self.held.file.read(&mut buffer).map_err(|_| {
                    ExistingImageFault::Resolve(resolution::ResolveFault::Unavailable)
                })?;
            if length == 0 {
                break;
            }
            bytes = bytes
                .checked_add(length as u64)
                .ok_or(ExistingImageFault::Unsupported)?;
            if bytes > expected {
                return Err(ExistingImageFault::Unsupported);
            }
            hash.update(&buffer[..length]);
        }
        check()?;
        if bytes != expected {
            return Err(ExistingImageFault::Unsupported);
        }
        Ok(HashedCli {
            held: self.held,
            bytes,
            digest: hash.finalize().into(),
        })
    }
}
/// Still non-executable. Only reviewed pins or fresh fixed-origin metadata may
/// authenticate the complete held image; package/version hints never do so.
pub(crate) struct HashedCli {
    held: resolution::ResolvedFile,
    bytes: u64,
    digest: [u8; 32],
}
impl HashedCli {
    pub(crate) fn matches_artifact(
        &self,
        artifact: &crate::release_admission::OriginalCliArtifact,
    ) -> bool {
        artifact.matches_image(self.bytes, &self.digest)
    }
    pub(crate) fn reference(&self) -> Option<crate::pin::ReviewedCli> {
        crate::pin::ReviewedCli::recognize(self.bytes, &self.digest)
    }
    pub(crate) fn admit_reference(self) -> Result<PinnedImage, ExistingImageFault> {
        let pin = self.reference().ok_or(ExistingImageFault::Unsupported)?;
        Ok(self.seal(Provenance::Reference(pin)))
    }
    pub(crate) fn admit_official(
        self,
        artifact: crate::release_admission::OriginalCliArtifact,
    ) -> Result<PinnedImage, ExistingImageFault> {
        // Bind the retained provenance tuple, not just an unlabelled digest.
        if artifact.release_id() == 0 || artifact.asset_id() == 0
            || artifact.tag() != format!("rust-v{}", artifact.version())
            || artifact.asset_url() != format!("https://github.com/openai/codex/releases/download/{}/codex-x86_64-pc-windows-msvc.exe", artifact.tag())
            || self.bytes != artifact.image_bytes() || &self.digest != artifact.image_sha256() {
            return Err(ExistingImageFault::Unsupported);
        }
        Ok(self.seal(Provenance::Official(artifact)))
    }
    fn seal(self, provenance: Provenance) -> PinnedImage {
        PinnedImage {
            kind: crate::pin::ImageKind::Cli,
            provenance,
            path: self.held.path,
            _file: self.held.file,
            _ancestors: self.held.pins,
        }
    }
}
