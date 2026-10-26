//! Windows held-image, private-root ACL and LPAC identity primitives.
use super::wide;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::Read,
    mem::{size_of, size_of_val},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, Prefix},
};
use windows::{
    core::{BOOL, PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            CopySid, CreateWellKnownSid, FreeSid, GetLengthSid, GetSecurityDescriptorDacl,
            GetSecurityDescriptorSacl, GetTokenInformation,
            Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile},
            TokenUser, WinCapabilityInternetClientSid, DACL_SECURITY_INFORMATION,
            LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
            SID_AND_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

const OPEN_NOFOLLOW_DIRECTORY: u32 = 0x0220_0000;
fn handle(file: &impl AsRawHandle) -> HANDLE {
    HANDLE(file.as_raw_handle())
}
fn sid_string(sid: PSID) -> Result<String, &'static str> {
    let mut text = PWSTR::null();
    // SAFETY: the SID is backed by a live owned token/profile allocation.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|_| "SID conversion failed")?;
    let result = unsafe { text.to_string() }.map_err(|_| "SID conversion failed");
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    result
}
fn owner_sid() -> Result<String, &'static str> {
    let mut raw = HANDLE::default();
    // SAFETY: token output is local and becomes uniquely owned immediately.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }
        .map_err(|_| "owner token unavailable")?;
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut buffer = [0usize; 128];
    let mut length = 0;
    unsafe {
        GetTokenInformation(
            handle(&token),
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size_of_val(&buffer) as u32,
            &mut length,
        )
    }
    .map_err(|_| "owner token unavailable")?;
    if length < size_of::<TOKEN_USER>() as u32 || length as usize > size_of_val(&buffer) {
        return Err("owner token invalid");
    }
    sid_string(unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}

pub(super) struct Profile {
    delete_on_drop: bool,
    name: Vec<u16>,
    sid: Vec<usize>,
    internet: Vec<usize>,
}
impl Profile {
    /// Only freshly generated identities are admitted by this candidate. Persistent
    /// vendor identity/retirement remains part of the production qualification.
    pub(super) fn create(name: &str) -> Result<Self, &'static str> {
        if !name.starts_with("Zephium.ClaudeFixture.")
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        {
            return Err("invalid private identity");
        }
        let name = wide(OsStr::new(name))?;
        // SAFETY: all UTF-16 strings are live, NUL-terminated; no capabilities yet.
        let sid = unsafe {
            CreateAppContainerProfile(
                PCWSTR(name.as_ptr()),
                PCWSTR(name.as_ptr()),
                PCWSTR(name.as_ptr()),
                None,
            )
        }
        .map_err(|error| {
            super::diagnostic("create-private-identity", error.code().0);
            "private identity unavailable"
        })?;
        let length = unsafe { GetLengthSid(sid) } as usize;
        if !(8..=128).contains(&length) {
            unsafe {
                FreeSid(sid);
                let _ = DeleteAppContainerProfile(PCWSTR(name.as_ptr()));
            }
            return Err("invalid private SID");
        }
        let mut storage = vec![0usize; length.div_ceil(size_of::<usize>())];
        let copied = unsafe { CopySid(length as u32, PSID(storage.as_mut_ptr().cast()), sid) };
        unsafe {
            FreeSid(sid);
        }
        if copied.is_err() {
            unsafe {
                let _ = DeleteAppContainerProfile(PCWSTR(name.as_ptr()));
            }
            return Err("private SID copy failed");
        }
        let mut result = Self {
            delete_on_drop: true,
            name,
            sid: storage,
            internet: vec![0usize; 16],
        };
        let mut bytes = size_of_val(result.internet.as_slice()) as u32;
        unsafe {
            CreateWellKnownSid(
                WinCapabilityInternetClientSid,
                None,
                Some(PSID(result.internet.as_mut_ptr().cast())),
                &mut bytes,
            )
        }
        .map_err(|_| "internetClient capability unavailable")?;
        Ok(result)
    }
    pub(super) fn close(mut self) -> Result<(), &'static str> {
        unsafe { DeleteAppContainerProfile(PCWSTR(self.name.as_ptr())) }
            .map_err(|_| "private identity cleanup failed")?;
        self.delete_on_drop = false;
        Ok(())
    }
    pub(super) fn with_capabilities<T>(
        &self,
        operation: impl FnOnce(&SECURITY_CAPABILITIES) -> T,
    ) -> T {
        let mut capability = SID_AND_ATTRIBUTES {
            Sid: PSID(self.internet.as_ptr().cast_mut().cast()),
            Attributes: 0x0000_0004, // SE_GROUP_ENABLED,
        };
        let capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: PSID(self.sid.as_ptr().cast_mut().cast()),
            Capabilities: &mut capability,
            CapabilityCount: 1,
            Reserved: 0,
        };
        operation(&capabilities)
    }
    pub(super) fn seal(&self, path: &Path, writable: bool) -> Result<(), &'static str> {
        let owner = owner_sid()?;
        let package = sid_string(PSID(self.sid.as_ptr().cast_mut().cast()))?;
        let access = if writable { "FA" } else { "GRGX" };
        // Host and SYSTEM own the directory. No ALL APPLICATION PACKAGES grant.
        let sddl = wide(OsStr::new(&format!("D:P(A;OICI;FA;;;{owner})(A;OICI;FA;;;SY)(A;OICI;{access};;;{package})S:(ML;OICI;NW;;;LW)")))?;
        let file = OpenOptions::new()
            .access_mode(0x000c_0080)
            .share_mode(0)
            .custom_flags(OPEN_NOFOLLOW_DIRECTORY)
            .open(path)
            .map_err(|error| {
                super::diagnostic("private-acl-open", error.raw_os_error().unwrap_or(-1));
                "private ACL target unavailable"
            })?;
        let metadata = file
            .metadata()
            .map_err(|_| "private metadata unavailable")?;
        if metadata.file_attributes() & 0x400 != 0
            || (!metadata.is_dir() && (!metadata.is_file() || !single_link(&file)?))
        {
            return Err("private ACL target is aliased");
        }
        let mut actual_owner = PSID::default();
        let mut current = PSECURITY_DESCRIPTOR::default();
        // SAFETY: security information is queried on the held, exclusive handle.
        unsafe {
            GetSecurityInfo(
                handle(&file),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                Some(&mut actual_owner),
                None,
                None,
                None,
                Some(&mut current),
            )
        }
        .ok()
        .map_err(|_| "private owner unavailable")?;
        let actual = sid_string(actual_owner);
        unsafe { LocalFree(Some(HLOCAL(current.0))) };
        if actual? != owner {
            return Err("private target owner mismatch");
        }
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|_| "private descriptor invalid")?;
        let result = assign(&file, descriptor);
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        result
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        if self.delete_on_drop {
            unsafe {
                let _ = DeleteAppContainerProfile(PCWSTR(self.name.as_ptr()));
            }
        }
    }
}
fn assign(file: &File, descriptor: PSECURITY_DESCRIPTOR) -> Result<(), &'static str> {
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut dacl = std::ptr::null_mut();
    let mut sacl = std::ptr::null_mut();
    // SAFETY: descriptor stays live until all ACL pointers have been consumed.
    unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
        .map_err(|_| "private DACL invalid")?;
    if !present.as_bool() || dacl.is_null() {
        return Err("private DACL missing");
    }
    unsafe { GetSecurityDescriptorSacl(descriptor, &mut present, &mut sacl, &mut defaulted) }
        .map_err(|_| "private label invalid")?;
    if !present.as_bool() || sacl.is_null() {
        return Err("private label missing");
    }
    unsafe {
        SetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION
                | LABEL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl),
            Some(sacl),
        )
    }
    .ok()
    .map_err(|error| {
        super::diagnostic("private-acl-assignment", error.code().0);
        "private ACL assignment failed"
    })
}
fn single_link(file: &File) -> Result<bool, &'static str> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle(file), &mut info) }
        .map_err(|_| "image link identity unavailable")?;
    Ok(info.nNumberOfLinks == 1)
}
fn plain_local_path(path: &Path) -> bool {
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)))
        || !matches!(parts.next(), Some(Component::RootDir))
    {
        return false;
    }
    parts.all(|part| match part {
        Component::Normal(value) => value
            .to_str()
            .is_some_and(|v| !v.is_empty() && !v.contains(':') && !v.ends_with(['.', ' '])),
        _ => false,
    })
}
/// File and all ancestors remain held against write/delete replacement through reap.
pub(super) struct HeldImage {
    pub(super) path: std::path::PathBuf,
    pub(super) file: File,
    _ancestors: Vec<File>,
}
impl HeldImage {
    pub(super) fn open(path: &Path, expected: [u8; 32]) -> Result<Self, &'static str> {
        if !plain_local_path(path) || path.extension().and_then(OsStr::to_str) != Some("exe") {
            return Err("only an exact local native executable is admitted");
        }
        let mut ancestors = Vec::new();
        let mut paths: Vec<_> = path.ancestors().skip(1).collect();
        paths.reverse();
        for directory in paths {
            let held = OpenOptions::new()
                .access_mode(0x80)
                .share_mode(3)
                .custom_flags(OPEN_NOFOLLOW_DIRECTORY)
                .open(directory)
                .map_err(|_| "image ancestor cannot be held")?;
            let metadata = held
                .metadata()
                .map_err(|_| "image ancestor metadata unavailable")?;
            if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                return Err("image ancestor is aliased");
            }
            ancestors.push(held);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(0x0020_0000)
            .open(path)
            .map_err(|_| "image cannot be held")?;
        let metadata = file.metadata().map_err(|_| "image metadata unavailable")?;
        if !metadata.is_file()
            || metadata.file_attributes() & 0x400 != 0
            || metadata.len() > 512 * 1024 * 1024
            || !single_link(&file)?
        {
            return Err("image identity rejected");
        }
        if hash(&mut file)? != expected {
            return Err("pinned image hash mismatch");
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            _ancestors: ancestors,
        })
    }
}
pub(super) fn hash(file: &mut File) -> Result<[u8; 32], &'static str> {
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    let mut total = 0usize;
    loop {
        let count = file
            .read(&mut bytes)
            .map_err(|_| "image hash read failed")?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count).ok_or("image too large")?;
        if total > 512 * 1024 * 1024 {
            return Err("image too large");
        }
        digest.update(&bytes[..count]);
    }
    Ok(digest.finalize().into())
}
