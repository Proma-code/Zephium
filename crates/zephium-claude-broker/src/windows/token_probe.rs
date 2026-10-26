//! Debug-only actual-token ACL measurements, not a production IsLPAC API.
use super::wide;
use std::{
    ffi::OsStr,
    mem::size_of_val,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows::{
    core::{BOOL, PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            AccessCheck,
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            DuplicateToken, SecurityIdentification, GENERIC_MAPPING, PRIVILEGE_SET,
            PSECURITY_DESCRIPTOR, SECURITY_CAPABILITIES, TOKEN_DUPLICATE, TOKEN_QUERY,
        },
        System::Threading::OpenProcessToken,
    },
};
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0 .0))) };
    }
}
pub(super) fn verify(
    process: HANDLE,
    capabilities: &SECURITY_CAPABILITIES,
    expect_lpac: bool,
) -> Result<(), &'static str> {
    let mut raw = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY | TOKEN_DUPLICATE, &mut raw) }.map_err(
        |error| {
            super::diagnostic("token-control-open", error.code().0);
            "token control unavailable"
        },
    )?;
    let primary = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut raw = HANDLE::default();
    unsafe {
        DuplicateToken(
            HANDLE(primary.as_raw_handle()),
            SecurityIdentification,
            &mut raw,
        )
    }
    .map_err(|error| {
        super::diagnostic("token-control-duplicate", error.code().0);
        "token control duplicate unavailable"
    })?;
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(capabilities.AppContainerSid, &mut sid) }
        .map_err(|_| "token control SID unavailable")?;
    let package = unsafe { sid.to_string() }.map_err(|_| "token control SID invalid");
    unsafe { LocalFree(Some(HLOCAL(sid.0.cast()))) };
    let package = package?;
    for (stage, dacl, expected) in [
        ("token-control-everyone", "(A;;0x1;;;WD)".to_owned(), false),
        (
            "token-control-package",
            format!("(A;;0x1;;;WD)(A;;0x1;;;{package})"),
            true,
        ),
        (
            "token-control-all-apps",
            "(A;;0x1;;;WD)(A;;0x1;;;AC)".to_owned(),
            !expect_lpac,
        ),
    ] {
        // SYSTEM owner/group cannot confer owner rights to this ordinary-user
        // child. Desired FILE_READ_DATA is concrete (no generic/max-access bits).
        // No descriptor rewriting and no filesystem ACL mutation occurs here.
        let sddl = wide(OsStr::new(&format!("O:SYG:SYD:P{dacl}")))?;
        let mut raw_descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut raw_descriptor,
                None,
            )
        }
        .map_err(|_| "token control descriptor unavailable")?;
        let descriptor = Descriptor(raw_descriptor);
        let mapping = GENERIC_MAPPING {
            GenericRead: 1,
            GenericWrite: 2,
            GenericExecute: 4,
            GenericAll: 7,
        };
        let mut privileges = [0u64; 128];
        let mut length = size_of_val(&privileges) as u32;
        let mut granted = 0u32;
        let mut allowed = BOOL(0);
        unsafe {
            AccessCheck(
                descriptor.0,
                HANDLE(token.as_raw_handle()),
                1,
                &mapping,
                Some(privileges.as_mut_ptr().cast::<PRIVILEGE_SET>()),
                &mut length,
                &mut granted,
                &mut allowed,
            )
        }
        .map_err(|error| {
            super::diagnostic("token-control-accesscheck", error.code().0);
            "token control API failed"
        })?;
        // API failure is never interpreted as denial; grants must be exact. No
        // dynamic allocation/retry can silently broaden privilege-set handling.
        if length < 8
            || length as usize > size_of_val(&privileges)
            || unsafe { (*privileges.as_ptr().cast::<PRIVILEGE_SET>()).PrivilegeCount } != 0
            || granted != u32::from(expected)
            || allowed.as_bool() != expected
        {
            super::diagnostic(stage, i32::from(allowed.as_bool()));
            super::diagnostic("token-control-granted", granted as i32);
            return Err("actual token ACL control mismatch");
        }
        super::diagnostic(stage, i32::from(allowed.as_bool()));
    }
    super::diagnostic("token-control-lpac-expected", i32::from(expect_lpac));
    Ok(())
}
