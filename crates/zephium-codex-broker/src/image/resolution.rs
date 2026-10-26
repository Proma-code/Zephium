//! Bounded local reparse discovery, followed by a held physical-file identity join.
//! Alias paths are hints, never execution authority. Unknown/remote/relative
//! reparses are refused before any target open; no shell or metadata script runs.
use super::*;
use ::windows::Win32::{
    Storage::FileSystem::{
        FileIdInfo, GetDriveTypeW, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
        QueryDosDeviceW, FILE_ID_INFO, GETFINALPATHNAMEBYHANDLE_FLAGS,
    },
    System::IO::DeviceIoControl,
};
use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
};

// GetDriveTypeW values from the Windows SDK; avoid an unrelated API module.
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const MAX_COMPONENTS: usize = 64;
const MAX_LINKS: usize = 8;
const GET_REPARSE: u32 = 0x0009_00a8;
const MOUNT_POINT: u32 = 0xa000_0003;
const SYMLINK: u32 = 0xa000_000c;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolveFault {
    Missing,
    Unavailable,
    Shape,
    Changed,
}
pub(crate) struct ResolvedFile {
    pub(super) path: PathBuf,
    pub(crate) file: File,
    pub(super) pins: Vec<File>,
    pub(super) identity: (u64, [u8; 16]),
}
fn io_fault(error: std::io::Error) -> ResolveFault {
    if error.kind() == std::io::ErrorKind::NotFound {
        ResolveFault::Missing
    } else {
        ResolveFault::Unavailable
    }
}
fn native_path(file: &File) -> Result<PathBuf, ResolveFault> {
    let mut value = vec![0; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            HANDLE(file.as_raw_handle()),
            &mut value,
            GETFINALPATHNAMEBYHANDLE_FLAGS(0),
        )
    } as usize;
    if length == 0 || length >= value.len() {
        return Err(ResolveFault::Shape);
    }
    let path = PathBuf::from(OsString::from_wide(&value[..length]));
    require_absolute_local(&path).map_err(|_| ResolveFault::Shape)?;
    Ok(path)
}
fn identity(file: &File) -> Result<(u64, [u8; 16]), ResolveFault> {
    let mut id = FILE_ID_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(|_| ResolveFault::Unavailable)?;
    Ok((id.VolumeSerialNumber, id.FileId.Identifier))
}

/// Component opens never follow the current component. Previously opened
/// directory handles exclude mutation while its next child is inspected.
pub(crate) fn local_file(candidate: &Path) -> Result<ResolvedFile, ResolveFault> {
    let mut current = candidate.to_owned();
    let mut continuity = Vec::new();
    for links in 0..=MAX_LINKS {
        require_absolute_local(&current).map_err(|_| ResolveFault::Shape)?;
        local_drive(&current)?;
        let chain: Vec<_> = current
            .ancestors()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if chain.len() > MAX_COMPONENTS {
            return Err(ResolveFault::Shape);
        }
        let mut restart = None;
        for (index, component) in chain.iter().enumerate() {
            let file = OpenOptions::new()
                .read(true)
                .share_mode(1)
                .custom_flags(OPEN_NOFOLLOW | OPEN_DIRECTORY)
                .open(component)
                .map_err(io_fault)?;
            let metadata = file.metadata().map_err(io_fault)?;
            if metadata.file_attributes() & REPARSE != 0 {
                if links == MAX_LINKS {
                    return Err(ResolveFault::Shape);
                }
                let target = reparse_target(&file)?;
                let remaining = current
                    .strip_prefix(component)
                    .map_err(|_| ResolveFault::Shape)?;
                let target = append_remaining(target, remaining);
                continuity.push(file);
                restart = Some(target);
                break;
            }
            if index + 1 != chain.len() {
                if !metadata.is_dir() {
                    return Err(ResolveFault::Shape);
                }
                continuity.push(file);
            } else {
                if !metadata.is_file() {
                    return Err(ResolveFault::Shape);
                }
                let physical = native_path(&file)?;
                let resolved = join_physical(&file, &physical)?;
                // The visible install selection is not held for the session:
                // only the independently joined physical image and ancestors.
                drop(continuity);
                return Ok(resolved);
            }
        }
        current = restart.ok_or(ResolveFault::Shape)?;
    }
    Err(ResolveFault::Shape)
}
fn append_remaining(mut target: PathBuf, remaining: &Path) -> PathBuf {
    if !remaining.as_os_str().is_empty() {
        target.push(remaining);
    }
    target
}
fn join_physical(initial: &File, path: &Path) -> Result<ResolvedFile, ResolveFault> {
    require_absolute_local(path).map_err(|_| ResolveFault::Shape)?;
    local_drive(path)?;
    let ancestors = path.ancestors().skip(1).collect::<Vec<_>>();
    if ancestors.len() > MAX_COMPONENTS {
        return Err(ResolveFault::Shape);
    }
    let mut pins = Vec::new();
    for ancestor in ancestors.into_iter().rev() {
        pins.push(pin_directory(ancestor).map_err(|_| ResolveFault::Shape)?);
    }
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(OPEN_NOFOLLOW)
        .open(path)
        .map_err(io_fault)?;
    let metadata = file.metadata().map_err(io_fault)?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE != 0 {
        return Err(ResolveFault::Shape);
    }
    let identity = identity(&file)?;
    if self::identity(initial)? != identity {
        return Err(ResolveFault::Changed);
    }
    Ok(ResolvedFile {
        path: path.to_owned(),
        file,
        pins,
        identity,
    })
}
// DOS spelling is not proof of local storage: mapped drives and SUBST aliases
// are refused using namespace metadata before any file-system candidate open.
pub(crate) fn local_drive(path: &Path) -> Result<(), ResolveFault> {
    let drive = match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => return Err(ResolveFault::Shape),
        },
        _ => return Err(ResolveFault::Shape),
    };
    let name = [u16::from(drive), 58, 0];
    let mut target = vec![0u16; 32768];
    let count =
        unsafe { QueryDosDeviceW(::windows::core::PCWSTR(name.as_ptr()), Some(&mut target)) }
            as usize;
    if count == 0 || count > target.len() {
        return Err(ResolveFault::Unavailable);
    }
    let end = target[..count]
        .iter()
        .position(|c| *c == 0)
        .ok_or(ResolveFault::Shape)?;
    let prefix: Vec<u16> = OsStr::new(r"\Device\HarddiskVolume")
        .encode_wide()
        .collect();
    let suffix = target[..end]
        .strip_prefix(prefix.as_slice())
        .ok_or(ResolveFault::Shape)?;
    if suffix.is_empty() || !suffix.iter().all(|c| (48..=57).contains(c)) {
        return Err(ResolveFault::Shape);
    }
    let root = [u16::from(drive), 58, 92, 0];
    if !matches!(
        unsafe { GetDriveTypeW(::windows::core::PCWSTR(root.as_ptr())) },
        DRIVE_FIXED | DRIVE_REMOVABLE
    ) {
        return Err(ResolveFault::Shape);
    }
    Ok(())
}
fn reparse_target(file: &File) -> Result<PathBuf, ResolveFault> {
    let mut buffer = [0u8; 16 * 1024];
    let mut returned = 0;
    unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            GET_REPARSE,
            None,
            0,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(|_| ResolveFault::Unavailable)?;
    parse_target(buffer.get(..returned as usize).ok_or(ResolveFault::Shape)?)
}
fn parse_target(bytes: &[u8]) -> Result<PathBuf, ResolveFault> {
    fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
    }
    fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    }
    let tag = u32_at(bytes, 0).ok_or(ResolveFault::Shape)?;
    let declared = usize::from(u16_at(bytes, 4).ok_or(ResolveFault::Shape)?);
    if declared.checked_add(8) != Some(bytes.len()) {
        return Err(ResolveFault::Shape);
    }
    let base = match tag {
        MOUNT_POINT => 16,
        SYMLINK if u32_at(bytes, 16) == Some(0) => 20,
        _ => return Err(ResolveFault::Shape),
    };
    let start = usize::from(u16_at(bytes, 8).ok_or(ResolveFault::Shape)?);
    let length = usize::from(u16_at(bytes, 10).ok_or(ResolveFault::Shape)?);
    if start % 2 != 0 || length == 0 || length % 2 != 0 {
        return Err(ResolveFault::Shape);
    }
    let start = base + start;
    let end = start.checked_add(length).ok_or(ResolveFault::Shape)?;
    let source = bytes.get(start..end).ok_or(ResolveFault::Shape)?;
    let units: Vec<u16> = source
        .chunks_exact(2)
        .map(|part| u16::from_le_bytes([part[0], part[1]]))
        .collect();
    if units.contains(&0)
        || units.len() < 7
        || units[..4] != [92, 63, 63, 92]
        || !((65..=90).contains(&units[4]) || (97..=122).contains(&units[4]))
        || units[5..7] != [58, 92]
    {
        return Err(ResolveFault::Shape);
    }
    let path = PathBuf::from(OsString::from_wide(&units[4..]));
    require_absolute_local(&path).map_err(|_| ResolveFault::Shape)?;
    Ok(path)
}

#[cfg(test)]
#[path = "resolution_tests.rs"]
mod tests;
