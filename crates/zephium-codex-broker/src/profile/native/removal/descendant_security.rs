//! Cleanup-only descendant ACL validation. Ordinary Codex-created children
//! inherit the owned parent's owner/SYSTEM grants and need not be PROTECTED.
//! Fixed root, journal and lease still use the exact original verifier.
use super::*;
use windows::Win32::Security::{TokenOwner, TOKEN_OWNER};

// Instrumentation exists only in explicit synthetic Windows fixture builds.
// It neither substitutes a security decision nor exposes a path/SID/ACL string.
macro_rules! diagnostic_stage {
    ($stage:expr) => {
        #[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
        diagnostic::stage($stage);
    };
}

pub(super) fn verify(file: &File, owner: &str, volume: u64) -> Result<bool, RemovalFault> {
    #[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
    let _diagnostic = diagnostic::start(file, owner, volume);
    diagnostic_stage!(1);
    let metadata = file.metadata().map_err(|_| RemovalFault::Storage)?;
    diagnostic_stage!(2);
    if metadata.file_attributes() & 0x400 != 0 || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(RemovalFault::Scope);
    }
    diagnostic_stage!(3);
    if identity(file).map_err(|_| RemovalFault::Scope)?.volume != volume {
        return Err(RemovalFault::Scope);
    }
    if metadata.is_file() {
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        diagnostic_stage!(4);
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
            .map_err(|_| RemovalFault::Scope)?;
        diagnostic_stage!(5);
        if information.nNumberOfLinks != 1 {
            return Err(RemovalFault::Scope);
        }
    }
    let mut actual_owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    diagnostic_stage!(6);
    unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut actual_owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    }
    .ok()
    .map_err(|_| RemovalFault::Scope)?;
    let descriptor = Descriptor(descriptor);
    diagnostic_stage!(7);
    validate_owner(actual_owner, owner)?;
    diagnostic_stage!(8);
    let bytes = acl_bytes(descriptor.0).map_err(|_| RemovalFault::Scope)?;
    validate_acl(&bytes, owner)?;
    diagnostic_stage!(255);
    Ok(metadata.is_dir())
}
// A normal child object uses the creator token's default owner, which need
// not be its TokenUser. This is a cleanup-only mapping of the current process
// token, never an arbitrary group/admin allowlist. The same queried token must
// first join the profile user. DACL trustees remain only that user and SYSTEM;
// fixed metadata still uses the original exact-owner verifier. A different
// historical default owner after a token/elevation change is not inferred.
fn current_default_owner(owner: &str) -> Result<String, RemovalFault> {
    let mut raw = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }
        .map_err(|_| RemovalFault::Scope)?;
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let handle = HANDLE(token.as_raw_handle());
    let mut user_buffer = [0usize; 128];
    let mut user_length = 0;
    unsafe {
        GetTokenInformation(
            handle,
            TokenUser,
            Some(user_buffer.as_mut_ptr().cast()),
            size_of_val(&user_buffer) as u32,
            &mut user_length,
        )
    }
    .map_err(|_| RemovalFault::Scope)?;
    if user_length < size_of::<TOKEN_USER>() as u32
        || user_length as usize > size_of_val(&user_buffer)
        || sid(unsafe { (*user_buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid })
            .map_err(|_| RemovalFault::Scope)?
            != owner
    {
        return Err(RemovalFault::Scope);
    }
    let mut owner_buffer = [0usize; 128];
    let mut owner_length = 0;
    unsafe {
        GetTokenInformation(
            handle,
            TokenOwner,
            Some(owner_buffer.as_mut_ptr().cast()),
            size_of_val(&owner_buffer) as u32,
            &mut owner_length,
        )
    }
    .map_err(|_| RemovalFault::Scope)?;
    if owner_length < size_of::<TOKEN_OWNER>() as u32
        || owner_length as usize > size_of_val(&owner_buffer)
    {
        return Err(RemovalFault::Scope);
    }
    sid(unsafe { (*owner_buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner })
        .map_err(|_| RemovalFault::Scope)
}
fn validate_owner(actual: PSID, owner: &str) -> Result<(), RemovalFault> {
    if actual.0.is_null() {
        return Err(RemovalFault::Scope);
    }
    let actual = sid(actual).map_err(|_| RemovalFault::Scope)?;
    let default_owner = current_default_owner(owner)?;
    if actual != owner && actual != default_owner {
        return Err(RemovalFault::Scope);
    }
    Ok(())
}
fn validate_acl(bytes: &[u8], owner: &str) -> Result<(), RemovalFault> {
    diagnostic_stage!(9);
    if bytes.len() < 8 {
        return Err(RemovalFault::Scope);
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    diagnostic_stage!(10);
    if !(1..=8).contains(&count) {
        return Err(RemovalFault::Scope);
    }
    let mut offset = 8usize;
    let mut effective_owner = false;
    for _ in 0..count {
        diagnostic_stage!(11);
        let header = bytes.get(offset..offset + 8).ok_or(RemovalFault::Scope)?;
        let size = u16::from_le_bytes([header[2], header[3]]) as usize;
        // Ordinary ACCESS_ALLOWED only; known OI/CI/NP/IO/INHERITED flags.
        diagnostic_stage!(12);
        if header[0] != 0 || header[1] & !0x1f != 0 || size < 16 || !size.is_multiple_of(4) {
            return Err(RemovalFault::Scope);
        }
        diagnostic_stage!(13);
        let end = offset.checked_add(size).ok_or(RemovalFault::Scope)?;
        let ace = bytes.get(offset..end).ok_or(RemovalFault::Scope)?;
        let mask = u32::from_le_bytes(ace[4..8].try_into().map_err(|_| RemovalFault::Scope)?);
        // File-specific/standard rights and generic file rights only. Unknown
        // trustee or ACE semantics are refused, never normalized/re-written.
        diagnostic_stage!(14);
        if mask == 0 || mask & !(0x001f_01ff | 0xf000_0000) != 0 {
            return Err(RemovalFault::Scope);
        }
        let sid_bytes = &ace[8..];
        diagnostic_stage!(15);
        if sid_bytes[0] != 1 || sid_bytes.len() != 8 + usize::from(sid_bytes[1]) * 4 {
            return Err(RemovalFault::Scope);
        }
        // Alignment is not assumed from Vec<u8>; copy into an aligned buffer.
        let mut aligned = [0u32; 20];
        diagnostic_stage!(16);
        if sid_bytes.len() > std::mem::size_of_val(&aligned) {
            return Err(RemovalFault::Scope);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                sid_bytes.as_ptr(),
                aligned.as_mut_ptr().cast::<u8>(),
                sid_bytes.len(),
            );
        }
        diagnostic_stage!(17);
        let value = sid(PSID(aligned.as_mut_ptr().cast())).map_err(|_| RemovalFault::Scope)?;
        diagnostic_stage!(18);
        if value != owner && value != "S-1-5-18" {
            return Err(RemovalFault::Scope);
        }
        effective_owner |= value == owner && header[1] & 0x08 == 0;
        offset = end;
    }
    diagnostic_stage!(19);
    if !effective_owner {
        return Err(RemovalFault::Scope);
    }
    // IsValidAcl plus the exact AceCount/size walk bounds all effective ACEs.
    // Remaining allocation slack is not an ACE and carries no grant semantics.
    Ok(())
}

#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod diagnostic {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static STAGE: Cell<u8> = const { Cell::new(0) };
        static LINES: Cell<u16> = const { Cell::new(0) };
    }
    // At most 257 fixed numeric lines per synthetic test thread, including a
    // final overflow marker. No unbounded descriptor/ACE/string projection.
    fn emit(kind: u8, values: [u64; 8]) {
        LINES.with(|lines| {
            let count = lines.get();
            if count < 256 {
                eprintln!("codex-descendant-diagnostic kind={kind} values={values:?}");
                lines.set(count + 1);
            } else if count == 256 {
                eprintln!("codex-descendant-diagnostic kind=254 values=[0, 0, 0, 0, 0, 0, 0, 0]");
                lines.set(count + 1);
            }
        });
    }
    fn api(site: u64, error: windows::core::Error) {
        emit(7, [site, error.code().0 as u32 as u64, 0, 0, 0, 0, 0, 0]);
    }
    pub(super) fn stage(value: u8) {
        STAGE.with(|stage| stage.set(value));
    }
    pub(super) struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            emit(0, [STAGE.with(Cell::get) as u64, 0, 0, 0, 0, 0, 0, 0]);
        }
    }
    fn token_owner() -> Option<String> {
        let mut raw = HANDLE::default();
        if let Err(error) = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }
        {
            api(1, error);
            return None;
        }
        let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        let mut buffer = [0usize; 128];
        let mut length = 0;
        if let Err(error) = unsafe {
            GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenOwner,
                Some(buffer.as_mut_ptr().cast()),
                size_of_val(&buffer) as u32,
                &mut length,
            )
        } {
            api(2, error);
            return None;
        }
        if length < size_of::<TOKEN_OWNER>() as u32 || length as usize > size_of_val(&buffer) {
            return None;
        }
        sid(unsafe { (*buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner }).ok()
    }
    pub(super) fn start(file: &File, owner: &str, volume: u64) -> Guard {
        stage(0);
        snapshot(file, owner, volume);
        Guard
    }
    fn snapshot(file: &File, owner: &str, volume: u64) {
        if let Ok(meta) = file.metadata() {
            let volume_equal = identity(file)
                .ok()
                .map(|id| u64::from(id.volume == volume))
                .unwrap_or(2);
            emit(
                1,
                [
                    meta.file_attributes() as u64,
                    u64::from(meta.is_dir()),
                    u64::from(meta.is_file()),
                    volume_equal,
                    0,
                    0,
                    0,
                    0,
                ],
            );
            if meta.is_file() {
                let mut info = BY_HANDLE_FILE_INFORMATION::default();
                match unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
                {
                    Ok(()) => emit(6, [info.nNumberOfLinks as u64, 0, 0, 0, 0, 0, 0, 0]),
                    Err(error) => api(3, error),
                }
            }
        }
        let mut actual_owner = PSID::default();
        let mut raw = PSECURITY_DESCRIPTOR::default();
        if let Err(error) = unsafe {
            GetSecurityInfo(
                HANDLE(file.as_raw_handle()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                Some(&mut actual_owner),
                None,
                None,
                None,
                Some(&mut raw),
            )
        }
        .ok()
        {
            api(4, error);
            return;
        }
        let descriptor = Descriptor(raw);
        let actual = sid(actual_owner).ok();
        let token_owner = token_owner();
        let token_user = owner_sid().ok();
        emit(
            2,
            [
                u64::from(actual.as_deref() == Some(owner)),
                u64::from(actual.is_some() && actual == token_owner),
                u64::from(actual.as_deref() == Some("S-1-5-18")),
                u64::from(token_user.as_deref() == Some(owner)),
                u64::from(token_owner.is_some()),
                u64::from(actual.is_some()),
                0,
                0,
            ],
        );
        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut acl: *mut ACL = std::ptr::null_mut();
        if let Err(error) = unsafe {
            GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted)
        } {
            api(5, error);
            return;
        }
        let valid = present.as_bool() && !acl.is_null() && unsafe { IsValidAcl(acl) }.as_bool();
        let (size, count, revision) = if valid {
            unsafe {
                (
                    (*acl).AclSize as usize,
                    (*acl).AceCount as usize,
                    (*acl).AclRevision,
                )
            }
        } else {
            (0, 0, 0)
        };
        emit(
            3,
            [
                u64::from(present.as_bool()),
                u64::from(defaulted.as_bool()),
                u64::from(acl.is_null()),
                u64::from(valid),
                size as u64,
                count as u64,
                revision as u64,
                0,
            ],
        );
        if !valid || !(size_of::<ACL>()..=4096).contains(&size) || count > 8 {
            return;
        }
        let bytes = unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), size) };
        let mut offset = 8usize;
        for index in 0..count {
            let Some(header) = bytes.get(offset..offset.saturating_add(8)) else {
                return;
            };
            let size = u16::from_le_bytes([header[2], header[3]]) as usize;
            let mask = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
            let Some(end) = offset.checked_add(size) else {
                return;
            };
            let Some(ace) = bytes.get(offset..end) else {
                return;
            };
            let value = if size >= 16 {
                let sid_bytes = &ace[8..];
                let mut aligned = [0u32; 20];
                if sid_bytes[0] == 1
                    && sid_bytes.len() == 8 + usize::from(sid_bytes[1]) * 4
                    && sid_bytes.len() <= size_of_val(&aligned)
                {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            sid_bytes.as_ptr(),
                            aligned.as_mut_ptr().cast::<u8>(),
                            sid_bytes.len(),
                        );
                    }
                    sid(PSID(aligned.as_mut_ptr().cast())).ok()
                } else {
                    None
                }
            } else {
                None
            };
            emit(
                4,
                [
                    index as u64,
                    header[0] as u64,
                    header[1] as u64,
                    size as u64,
                    mask as u64,
                    u64::from(value.as_deref() == Some(owner)),
                    u64::from(value.as_deref() == Some("S-1-5-18")),
                    u64::from(value.is_some()),
                ],
            );
            if size < 8 {
                return;
            }
            offset = end;
        }
    }
}

#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod tests {
    use super::*;
    fn held(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .share_mode(5)
            .custom_flags(0x0220_0000)
            .open(path)
            .unwrap()
    }
    fn descriptor(sddl: &str) -> Descriptor {
        let sddl = wide(OsStr::new(sddl)).unwrap();
        let mut raw = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )
        }
        .unwrap();
        Descriptor(raw)
    }
    fn new_file(path: &Path, security: &Descriptor) -> File {
        let path = wide(path.as_os_str()).unwrap();
        let raw = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                0x8000_0000,
                FILE_SHARE_MODE(5),
                Some(&security.attrs()),
                CREATE_NEW,
                FILE_FLAGS_AND_ATTRIBUTES(0x0020_0000),
                None,
            )
        }
        .unwrap();
        unsafe { File::from_raw_handle(raw.0) }
    }
    #[test]
    #[ignore = "owned Windows inherited-child ACL controls; no credentials or deletion"]
    fn normal_inherited_children_are_accepted_but_broad_null_and_hardlink_children_refuse() {
        let root = tempfile::tempdir().unwrap();
        let owner = owner_sid().unwrap();
        let parent = root.path().join("protected-owned-parent");
        create_dir(&parent, &owner).unwrap();
        // Prepare the dedicated real alias before the restrictive parent pin:
        // constructing test data is not a mutation of a held cleanup namespace.
        let linked_file = parent.join("hardlink-source");
        let second_link = parent.join("second-link");
        std::fs::write(&linked_file, b"synthetic-linked").unwrap();
        std::fs::hard_link(&linked_file, &second_link).unwrap();
        let parent_pin = held(&parent);
        let volume = identity(&parent_pin).unwrap().volume;
        let nested = parent.join("inherited-child");
        std::fs::create_dir(&nested).unwrap();
        let file = nested.join("nonsecret-data");
        std::fs::write(&file, b"synthetic").unwrap();
        assert_eq!(verify(&held(&nested), &owner, volume), Ok(true));
        assert_eq!(verify(&held(&file), &owner, volume), Ok(false));
        // Descriptor-only owner logic controls: no ownership-changing privilege
        // is enabled and no filesystem owner is rewritten for these negatives.
        let default_owner = current_default_owner(&owner).unwrap();
        let foreign = ["S-1-0-0", "S-1-5-7", "S-1-5-32-546"]
            .into_iter()
            .find(|candidate| *candidate != owner && *candidate != default_owner)
            .unwrap();
        let foreign_descriptor =
            descriptor(&format!("O:{foreign}D:P(A;;FA;;;{owner})(A;;FA;;;SY)"));
        let mut foreign_owner = PSID::default();
        let mut defaulted = BOOL::default();
        unsafe {
            windows::Win32::Security::GetSecurityDescriptorOwner(
                foreign_descriptor.0,
                &mut foreign_owner,
                &mut defaulted,
            )
        }
        .unwrap();
        assert_eq!(
            validate_acl(&acl_bytes(foreign_descriptor.0).unwrap(), &owner),
            Ok(())
        );
        assert_eq!(
            validate_owner(foreign_owner, &owner),
            Err(RemovalFault::Scope)
        );
        assert_eq!(current_default_owner(foreign), Err(RemovalFault::Scope));
        assert_eq!(
            validate_owner(PSID::default(), &owner),
            Err(RemovalFault::Scope)
        );
        let mut malformed = [0u32; 20];
        assert_eq!(
            validate_owner(PSID(malformed.as_mut_ptr().cast()), &owner),
            Err(RemovalFault::Scope)
        );
        let broad = new_file(
            &parent.join("broad-nonsecret"),
            &descriptor(&format!(
                "O:{owner}D:P(A;;FA;;;{owner})(A;;FA;;;SY)(A;;FR;;;WD)"
            )),
        );
        assert_eq!(verify(&broad, &owner, volume), Err(RemovalFault::Scope));
        let null = new_file(
            &parent.join("null-nonsecret"),
            &descriptor(&format!("O:{owner}D:NO_ACCESS_CONTROL")),
        );
        assert_eq!(verify(&null, &owner, volume), Err(RemovalFault::Scope));
        let source_pin = held(&linked_file);
        let alias_pin = held(&second_link);
        assert_eq!(
            identity(&source_pin).unwrap(),
            identity(&alias_pin).unwrap()
        );
        let mut links = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(source_pin.as_raw_handle()), &mut links) }
            .unwrap();
        assert_eq!(links.nNumberOfLinks, 2);
        assert_eq!(
            verify(&source_pin, &owner, volume),
            Err(RemovalFault::Scope)
        );
        drop(alias_pin);
        drop(source_pin);
        drop(broad);
        drop(null);
        drop(parent_pin);
        root.close().unwrap();
    }
}
