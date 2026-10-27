//! Cleanup-only descendant ACL validation. Ordinary Codex-created children
//! inherit the owned parent's owner/SYSTEM grants and need not be PROTECTED.
//! Fixed root, journal and lease still use the exact original verifier.
use super::*;
use windows::Win32::Security::{TokenOwner, TOKEN_OWNER};

pub(super) fn verify(file: &File, owner: &str, volume: u64) -> Result<bool, RemovalFault> {
    let metadata = file.metadata().map_err(|_| RemovalFault::Storage)?;
    if metadata.file_attributes() & 0x400 != 0 || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(RemovalFault::Scope);
    }
    if identity(file).map_err(|_| RemovalFault::Scope)?.volume != volume {
        return Err(RemovalFault::Scope);
    }
    if metadata.is_file() {
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
            .map_err(|_| RemovalFault::Scope)?;
        if information.nNumberOfLinks != 1 {
            return Err(RemovalFault::Scope);
        }
    }
    let mut actual_owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
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
    validate_owner(actual_owner, owner)?;
    let bytes = acl_bytes(descriptor.0).map_err(|_| RemovalFault::Scope)?;
    validate_acl(&bytes, owner)?;
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
    if bytes.len() < 8 {
        return Err(RemovalFault::Scope);
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if !(1..=8).contains(&count) {
        return Err(RemovalFault::Scope);
    }
    let mut offset = 8usize;
    let mut effective_owner = false;
    for _ in 0..count {
        let header = bytes.get(offset..offset + 8).ok_or(RemovalFault::Scope)?;
        let size = u16::from_le_bytes([header[2], header[3]]) as usize;
        // Ordinary ACCESS_ALLOWED only; known OI/CI/NP/IO/INHERITED flags.
        if header[0] != 0 || header[1] & !0x1f != 0 || size < 16 || !size.is_multiple_of(4) {
            return Err(RemovalFault::Scope);
        }
        let end = offset.checked_add(size).ok_or(RemovalFault::Scope)?;
        let ace = bytes.get(offset..end).ok_or(RemovalFault::Scope)?;
        let mask = u32::from_le_bytes(ace[4..8].try_into().map_err(|_| RemovalFault::Scope)?);
        // File-specific/standard rights and generic file rights only. Unknown
        // trustee or ACE semantics are refused, never normalized/re-written.
        if mask == 0 || mask & !(0x001f_01ff | 0xf000_0000) != 0 {
            return Err(RemovalFault::Scope);
        }
        let sid_bytes = &ace[8..];
        if sid_bytes[0] != 1 || sid_bytes.len() != 8 + usize::from(sid_bytes[1]) * 4 {
            return Err(RemovalFault::Scope);
        }
        // Alignment is not assumed from Vec<u8>; copy into an aligned buffer.
        let mut aligned = [0u32; 20];
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
        let value = sid(PSID(aligned.as_mut_ptr().cast())).map_err(|_| RemovalFault::Scope)?;
        if value != owner && value != "S-1-5-18" {
            return Err(RemovalFault::Scope);
        }
        effective_owner |= value == owner && header[1] & 0x08 == 0;
        offset = end;
    }
    if !effective_owner {
        return Err(RemovalFault::Scope);
    }
    // IsValidAcl plus the exact AceCount/size walk bounds all effective ACEs.
    // Remaining allocation slack is not an ACE and carries no grant semantics.
    Ok(())
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
