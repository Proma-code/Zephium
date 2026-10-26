//! Narrow held-native operations for synthetic account-store fixtures only.
use super::Error;
use std::{
    ffi::{c_void, OsStr},
    fs::{File, OpenOptions},
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
};
use windows::{
    core::{BOOL, PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL, UNICODE_STRING},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SE_FILE_OBJECT,
            },
            Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
            GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            GetSecurityDescriptorOwner, GetSecurityDescriptorSacl, IsValidSid, ACCESS_ALLOWED_ACE,
            ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        },
        Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, LockFileEx,
            BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, GETFINALPATHNAMEBYHANDLE_FLAGS,
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
        },
        System::IO::{IO_STATUS_BLOCK, OVERLAPPED},
    },
};

const READ: u32 = 0x0012_0089;
const WRITE: u32 = 0x0012_0116;
const READ_EXECUTE: u32 = 0x0012_00a9;
const ALL: u32 = 0x001f_01ff;
// Compile-time safety contract: package data authority must never include
// security administration, root deletion or delete-child authority.
const PACKAGE_DIRECTORY_DATA: u32 = READ | WRITE | 0x20; // Directory traverse, not deletion.
const PACKAGE_FILE_DATA: u32 = READ | WRITE; // No execute or atomic-replace DELETE yet.
const OWNER_READ_CONTROL: u32 = 0x0002_0000;
const _: () = assert!(PACKAGE_DIRECTORY_DATA & 0x000d_0040 == 0);
const _: () = assert!(PACKAGE_FILE_DATA & 0x000d_0060 == 0);
const OPEN: u32 = 1;
const CREATE: u32 = 2;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct Identity {
    volume: u64,
    file: [u8; 16],
}
#[derive(Clone, Eq, PartialEq)]
struct Ace {
    kind: u8,
    flags: u8,
    mask: u32,
    sid: String,
}
#[derive(Clone, Eq, PartialEq)]
struct Snapshot {
    owner: String,
    control: u16,
    dacl: Vec<Ace>,
    labels: Vec<Ace>,
}
#[derive(Clone, Eq, PartialEq)]
pub(super) struct DirectoryReceipt {
    name: String,
    identity: Identity,
    security: Snapshot,
}

pub(super) struct HeldDirectory {
    pub(super) file: File,
    pub(super) identity: Identity,
    name: String,
    security: Snapshot,
}
impl HeldDirectory {
    pub(super) fn receipt(&self) -> DirectoryReceipt {
        DirectoryReceipt {
            name: self.name.clone(),
            identity: self.identity,
            security: self.security.clone(),
        }
    }
}

pub(super) struct Policy {
    sddl: String,
}
impl Policy {
    pub(super) fn directory(owner: &str, package: &str, writable: bool) -> Self {
        let directory = if writable {
            PACKAGE_DIRECTORY_DATA
        } else {
            READ_EXECUTE
        };
        let file = if writable { PACKAGE_FILE_DATA } else { READ };
        // OWNER RIGHTS suppresses owner-implicit READ_CONTROL/WRITE_DAC. Only
        // explicit broker user/SYSTEM entries retain ACL administration. Actual
        // LPAC user/package-intersection denial still needs native qualification.
        // Separate file inheritance avoids granting FILE_EXECUTE via the same
        // bit used for directory traverse. No package DELETE/DELETE_CHILD grant
        // is assumed necessary before official-client compatibility is measured.
        Self { sddl: format!("O:{owner}D:P(A;OICI;0x{ALL:08x};;;{owner})(A;OICI;0x{ALL:08x};;;SY)(A;OICI;0x{OWNER_READ_CONTROL:08x};;;OW)(A;CI;0x{directory:08x};;;{package})(A;OIIO;0x{file:08x};;;{package})S:(ML;OICI;NW;;;LW)") }
    }
    pub(super) fn broker_file(owner: &str) -> Self {
        Self {
            sddl: format!(
                "O:{owner}D:P(A;;0x{ALL:08x};;;{owner})(A;;0x{ALL:08x};;;SY)(A;;0x{OWNER_READ_CONTROL:08x};;;OW)S:(ML;;NW;;;ME)"
            ),
        }
    }
    fn descriptor(&self) -> Result<Descriptor, Error> {
        let wide = super::super::wide(OsStr::new(&self.sddl)).map_err(|_| Error::Invalid)?;
        let mut raw = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                1,
                &mut raw,
                None,
            )
        }
        .map_err(|_| Error::Invalid)?;
        if raw.0.is_null() {
            return Err(Error::Invalid);
        }
        Ok(Descriptor(raw))
    }
    fn verify(&self, file: &File) -> Result<Snapshot, Error> {
        let expected = snapshot_descriptor(self.descriptor()?.0)?;
        let actual = snapshot(file)?;
        if actual.owner != expected.owner
            || actual.dacl != expected.dacl
            || actual.labels != expected.labels
            || actual.control & 0x1000 == 0
        {
            return Err(Error::Invalid);
        }
        Ok(actual)
    }
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0 .0))) };
    }
}
fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *const UNICODE_STRING,
    attributes: u32,
    security_descriptor: *const c_void,
    security_quality_of_service: *const c_void,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        file: *mut HANDLE,
        access: u32,
        attributes: *const ObjectAttributes,
        status: *mut IO_STATUS_BLOCK,
        allocation: *const i64,
        file_attributes: u32,
        share: u32,
        disposition: u32,
        options: u32,
        ea: *const c_void,
        ea_length: u32,
    ) -> i32;
    fn NtFlushBuffersFile(file: HANDLE, status: *mut IO_STATUS_BLOCK) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}
fn component(name: &str) -> Result<Vec<u16>, Error> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '\0'))
    {
        return Err(Error::Invalid);
    }
    let value: Vec<u16> = name.encode_utf16().collect();
    if value.len() > 255 {
        return Err(Error::Invalid);
    }
    Ok(value)
}
fn open(
    parent: &File,
    name: &str,
    directory: bool,
    access: u32,
    disposition: u32,
    descriptor: Option<&Descriptor>,
) -> Result<File, Error> {
    let mut name = component(name)?;
    let string = UNICODE_STRING {
        Length: (name.len() * 2) as u16,
        MaximumLength: (name.len() * 2) as u16,
        Buffer: PWSTR(name.as_mut_ptr()),
    };
    let attributes = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: handle(parent),
        object_name: &string,
        attributes: 0x40 | 0x1000, // OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE
        security_descriptor: descriptor.map_or(std::ptr::null(), |sd| sd.0 .0),
        security_quality_of_service: std::ptr::null(),
    };
    let mut raw = HANDLE::default();
    let mut status = IO_STATUS_BLOCK::default();
    // Synchronous, write-through, no-reparse, single-component held-parent open.
    // Share read/write but never delete: every admitted identity stays pinned.
    let result = unsafe {
        NtCreateFile(
            &mut raw,
            access | 0x0010_0000 | if directory { 0x20 } else { 0 },
            &attributes,
            &mut status,
            std::ptr::null(),
            0x80,
            3,
            disposition,
            (if directory { 1 } else { 0x40 }) | 0x20 | 0x0020_0000 | 2,
            std::ptr::null(),
            0,
        )
    };
    if result < 0 {
        return Err(match unsafe { RtlNtStatusToDosError(result) } {
            80 | 183 => Error::Collision,
            32 | 33 => Error::Busy,
            _ => Error::Io,
        });
    }
    if raw.is_invalid() {
        return Err(Error::Invalid);
    }
    let file = unsafe { File::from_raw_handle(raw.0) };
    if result != 0 {
        return Err(Error::Invalid);
    }
    identity(&file, directory)?;
    Ok(file)
}
pub(super) fn identity(file: &File, directory: bool) -> Result<Identity, Error> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle(file), &mut info) }.map_err(|_| Error::Invalid)?;
    if info.dwFileAttributes & 0x400 != 0
        || (info.dwFileAttributes & 0x10 != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(Error::Invalid);
    }
    let mut id = FILE_ID_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            handle(file),
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(|_| Error::Invalid)?;
    Ok(Identity {
        volume: id.VolumeSerialNumber,
        file: id.FileId.Identifier,
    })
}
fn normalized(file: &File) -> Result<String, Error> {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(handle(file), &mut buffer, GETFINALPATHNAMEBYHANDLE_FLAGS(0))
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(Error::Invalid);
    }
    String::from_utf16(&buffer[..length]).map_err(|_| Error::Invalid)
}
pub(super) fn canonical_parent(file: &File) -> Result<PathBuf, Error> {
    let normalized = normalized(file)?;
    let plain = normalized.strip_prefix("\\\\?\\").ok_or(Error::Invalid)?;
    Ok(PathBuf::from(plain))
}

pub(super) fn exact_parent_spelling(path: &Path, canonical: &Path) -> bool {
    // Path equality/components normalize separators and dots; compare the raw
    // UTF-16 instead. Admit only the OS's ordinary single trailing backslash.
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    let expected: Vec<u16> = canonical.as_os_str().encode_wide().collect();
    raw == expected
        || (raw.len() == expected.len() + 1
            && raw.last() == Some(&(b'\\' as u16))
            && raw[..expected.len()] == expected)
}

fn exact_name(file: &File, name: &str) -> Result<(), Error> {
    if normalized(file)?.rsplit('\\').next() != Some(name) {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn ntfs(file: &File) -> Result<(), Error> {
    let mut fs = [0u16; 32];
    unsafe { GetVolumeInformationByHandleW(handle(file), None, None, None, None, Some(&mut fs)) }
        .map_err(|_| Error::Invalid)?;
    if fs[..5] != [b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16, 0] {
        return Err(Error::Invalid);
    }
    // A local drive-root spelling is separately proved; UNC/device roots refuse.
    Ok(())
}
pub(super) fn fresh_name() -> Result<String, Error> {
    let mut random = [0u8; 16];
    if unsafe { BCryptGenRandom(None, &mut random, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.0 < 0 {
        return Err(Error::Io);
    }
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("Zephium.ClaudeStoreFixture.{suffix}"))
}
fn open_drive(drive: u8, owner: &str) -> Result<HeldDirectory, Error> {
    let name = format!("{}:\\", char::from(drive));
    let file = OpenOptions::new()
        .access_mode(READ | 0x20)
        .share_mode(3)
        .custom_flags(0x0220_0000)
        .open(&name)
        .map_err(|_| Error::Invalid)?;
    let identity = identity(&file, true)?;
    if !normalized(&file)?.eq_ignore_ascii_case(&format!("\\\\?\\{name}")) {
        return Err(Error::Invalid);
    }
    ntfs(&file)?;
    let security = ancestor(&file, owner)?;
    Ok(HeldDirectory {
        file,
        identity,
        name,
        security,
    })
}
pub(super) fn pin_parent(path: &Path, owner: &str) -> Result<Vec<HeldDirectory>, Error> {
    let mut components = path.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) => drive,
            _ => return Err(Error::Invalid),
        },
        _ => return Err(Error::Invalid),
    };
    if components.next() != Some(Component::RootDir) {
        return Err(Error::Invalid);
    }
    let names = components
        .map(|p| match p {
            Component::Normal(name) => name.to_str().ok_or(Error::Invalid),
            _ => Err(Error::Invalid),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names.is_empty() {
        return Err(Error::Invalid);
    }
    let mut chain = vec![open_drive(drive, owner)?];
    for (index, name) in names.iter().enumerate() {
        let file = open(
            &chain.last().ok_or(Error::Invalid)?.file,
            name,
            true,
            READ | if index + 1 == names.len() { WRITE } else { 0 },
            OPEN,
            None,
        )?;
        exact_name(&file, name)?;
        let security = ancestor(&file, owner)?;
        chain.push(HeldDirectory {
            identity: identity(&file, true)?,
            file,
            name: (*name).to_owned(),
            security,
        });
    }
    let canonical = canonical_parent(&chain.last().ok_or(Error::Invalid)?.file)?;
    if !exact_parent_spelling(path, &canonical) {
        return Err(Error::Invalid);
    }
    Ok(chain)
}
pub(super) fn create_directory(
    parent: &File,
    name: &str,
    policy: &Policy,
) -> Result<HeldDirectory, Error> {
    directory(parent, name, policy, CREATE)
}
pub(super) fn open_directory(
    parent: &File,
    name: &str,
    policy: &Policy,
) -> Result<HeldDirectory, Error> {
    directory(parent, name, policy, OPEN)
}
fn directory(
    parent: &File,
    name: &str,
    policy: &Policy,
    disposition: u32,
) -> Result<HeldDirectory, Error> {
    let descriptor = if disposition == CREATE {
        Some(policy.descriptor()?)
    } else {
        None
    };
    let file = open(
        parent,
        name,
        true,
        READ | WRITE,
        disposition,
        descriptor.as_ref(),
    )?;
    exact_name(&file, name)?;
    let security = policy.verify(&file)?;
    Ok(HeldDirectory {
        identity: identity(&file, true)?,
        file,
        name: name.to_owned(),
        security,
    })
}
pub(super) fn create_file(parent: &File, name: &str, policy: &Policy) -> Result<File, Error> {
    let file = open(
        parent,
        name,
        false,
        READ | WRITE,
        CREATE,
        Some(&policy.descriptor()?),
    )?;
    exact_name(&file, name)?;
    policy.verify(&file)?;
    file.sync_all().map_err(|_| Error::Io)?;
    Ok(file)
}
pub(super) fn open_file(parent: &File, name: &str, policy: &Policy) -> Result<File, Error> {
    let file = open(parent, name, false, READ | WRITE, OPEN, None)?;
    exact_name(&file, name)?;
    policy.verify(&file)?;
    Ok(file)
}
pub(super) fn verify_chain(
    chain: &[HeldDirectory],
    receipts: &[DirectoryReceipt],
    owner: &str,
) -> Result<(), Error> {
    if chain.len() != receipts.len() || chain.is_empty() {
        return Err(Error::Invalid);
    }
    let first = &chain[0];
    let drive = first
        .name
        .as_bytes()
        .first()
        .copied()
        .ok_or(Error::Invalid)?;
    if open_drive(drive, owner)?.identity != first.identity {
        return Err(Error::Invalid);
    }
    for (index, (directory, expected)) in chain.iter().zip(receipts).enumerate() {
        if directory.receipt() != *expected
            || identity(&directory.file, true)? != directory.identity
            || snapshot(&directory.file)? != directory.security
        {
            return Err(Error::Invalid);
        }
        if index > 0 {
            exact_name(&directory.file, &directory.name)?;
            let current = open(
                &chain[index - 1].file,
                &directory.name,
                true,
                READ,
                OPEN,
                None,
            )?;
            if identity(&current, true)? != directory.identity {
                return Err(Error::Invalid);
            }
        }
    }
    Ok(())
}
pub(super) fn verify_directory(
    parent: &File,
    name: &str,
    held: &HeldDirectory,
    policy: &Policy,
) -> Result<(), Error> {
    exact_name(&held.file, name)?;
    if identity(&held.file, true)? != held.identity || policy.verify(&held.file)? != held.security {
        return Err(Error::Invalid);
    }
    let current = open(parent, name, true, READ, OPEN, None)?;
    if identity(&current, true)? != held.identity {
        return Err(Error::Invalid);
    }
    Ok(())
}
pub(super) fn verify_file(
    parent: &File,
    name: &str,
    held: &File,
    expected: Identity,
    policy: &Policy,
) -> Result<(), Error> {
    exact_name(held, name)?;
    if identity(held, false)? != expected {
        return Err(Error::Invalid);
    }
    policy.verify(held)?;
    let current = open(parent, name, false, READ, OPEN, None)?;
    if identity(&current, false)? != expected {
        return Err(Error::Invalid);
    }
    Ok(())
}
pub(super) fn lock_exclusive(file: &File) -> Result<(), Error> {
    let mut overlapped = OVERLAPPED::default();
    unsafe {
        LockFileEx(
            handle(file),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            None,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    }
    .map_err(|error| {
        if error.code().0 as u32 == 0x8007_0021 {
            Error::Busy
        } else {
            Error::Io
        }
    })
}
pub(super) fn sync_directory(file: &File) -> Result<(), Error> {
    let mut status = IO_STATUS_BLOCK::default();
    if unsafe { NtFlushBuffersFile(handle(file), &mut status) } != 0 {
        return Err(Error::Io);
    }
    Ok(())
}
fn sid_string(sid: PSID) -> Result<String, Error> {
    if sid.0.is_null() || !unsafe { IsValidSid(sid) }.as_bool() {
        return Err(Error::Invalid);
    }
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|_| Error::Invalid)?;
    let result = unsafe { text.to_string() }.map_err(|_| Error::Invalid);
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    result
}
fn acl_entries(acl: *mut ACL) -> Result<Vec<Ace>, Error> {
    if acl.is_null() {
        return Ok(Vec::new());
    }
    let base = acl as usize;
    let size = usize::from(unsafe { (*acl).AclSize });
    let count = u32::from(unsafe { (*acl).AceCount });
    if size < size_of::<ACL>() || count > 256 {
        return Err(Error::Invalid);
    }
    let end = base.checked_add(size).ok_or(Error::Invalid)?;
    let mut entries = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut raw: *mut c_void = std::ptr::null_mut();
        unsafe { GetAce(acl, index, &mut raw) }.map_err(|_| Error::Invalid)?;
        let address = raw as usize;
        if address < base + size_of::<ACL>()
            || address
                .checked_add(size_of::<ACE_HEADER>())
                .is_none_or(|v| v > end)
            || !address.is_multiple_of(std::mem::align_of::<ACCESS_ALLOWED_ACE>())
        {
            return Err(Error::Invalid);
        }
        let header = unsafe { &*raw.cast::<ACE_HEADER>() };
        let length = usize::from(header.AceSize);
        let offset = offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        if !matches!(header.AceType, 0 | 1 | 17)
            || length < offset + 8
            || address.checked_add(length).is_none_or(|v| v > end)
        {
            return Err(Error::Invalid);
        }
        let sid = unsafe { raw.cast::<u8>().add(offset) };
        let subauthorities = unsafe { *sid.add(1) } as usize;
        if subauthorities > 15 || offset + 8 + 4 * subauthorities > length {
            return Err(Error::Invalid);
        }
        entries.push(Ace {
            kind: header.AceType,
            flags: header.AceFlags,
            mask: unsafe { (*raw.cast::<ACCESS_ALLOWED_ACE>()).Mask },
            sid: sid_string(PSID(sid.cast()))?,
        });
    }
    Ok(entries)
}
fn snapshot(file: &File) -> Result<Snapshot, Error> {
    let mut raw = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut raw),
        )
    }
    .ok()
    .map_err(|_| Error::Invalid)?;
    if raw.0.is_null() {
        return Err(Error::Invalid);
    }
    let descriptor = Descriptor(raw);
    snapshot_descriptor(descriptor.0)
}
fn snapshot_descriptor(sd: PSECURITY_DESCRIPTOR) -> Result<Snapshot, Error> {
    let mut owner = PSID::default();
    let mut defaulted = BOOL::default();
    unsafe { GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted) }
        .map_err(|_| Error::Invalid)?;
    let mut control = 0;
    let mut revision = 0;
    unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) }
        .map_err(|_| Error::Invalid)?;
    let mut present = BOOL::default();
    let mut dacl = std::ptr::null_mut();
    unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) }
        .map_err(|_| Error::Invalid)?;
    if !present.as_bool() || dacl.is_null() {
        return Err(Error::Invalid);
    }
    let dacl = acl_entries(dacl)?;
    let mut labels = std::ptr::null_mut();
    unsafe { GetSecurityDescriptorSacl(sd, &mut present, &mut labels, &mut defaulted) }
        .map_err(|_| Error::Invalid)?;
    let labels = if present.as_bool() {
        acl_entries(labels)?
    } else {
        Vec::new()
    };
    Ok(Snapshot {
        owner: sid_string(owner)?,
        control,
        dacl,
        labels,
    })
}
fn trusted(sid: &str, owner: &str) -> bool {
    sid == owner
        || matches!(
            sid,
            "S-1-5-18"
                | "S-1-5-32-544"
                | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        )
}
fn ancestor(file: &File, owner: &str) -> Result<Snapshot, Error> {
    let security = snapshot(file)?;
    if !trusted(&security.owner, owner) {
        return Err(Error::Invalid);
    }
    for entry in &security.dacl {
        if !matches!(entry.kind, 0 | 1) {
            return Err(Error::Invalid);
        }
        if entry.kind == 1 || entry.flags & 8 != 0 || entry.mask & (0x000d_0150 | 0x5000_0000) == 0
        {
            continue;
        }
        if !trusted(&entry.sid, owner) {
            return Err(Error::Invalid);
        }
    }
    Ok(security)
}
