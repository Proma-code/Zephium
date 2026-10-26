//! Independent Authenticode validation on the held, hash-pinned image.
use super::{security::HeldImage, wide};
use std::{mem::size_of, os::windows::io::AsRawHandle};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, HWND},
        Security::{
            Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE},
            WinTrust::*,
        },
    },
};

pub(super) const CANDIDATE_VERSION: &str = "2.1.310";
// Official manifest.json signature verified against documented release key
// 31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE. This is a fixture pin, not production
// availability: neither signed provenance nor --help proves account confinement.
pub(super) const CANDIDATE_SHA256: [u8; 32] = [
    0xf9, 0xa3, 0x92, 0x06, 0x34, 0xae, 0x3b, 0x96, 0x1e, 0xe9, 0x2c, 0x2e, 0xb5, 0x8d, 0x00, 0x51,
    0xf5, 0xf8, 0x85, 0x2b, 0x12, 0x09, 0x83, 0xd0, 0xa4, 0xf5, 0x7b, 0x6a, 0x5e, 0xfe, 0xba, 0x61,
];
/// Non-Clone held vendor-image authority. The only constructor fixes both the
/// release hash and publisher verification; callers cannot supply a trust flag,
/// alternate hash, mutable executable path or bare executable string to a Port.
pub(super) struct VerifiedImage(HeldImage);
impl VerifiedImage {
    pub(super) fn open(native_path: &std::path::Path) -> Result<Self, &'static str> {
        let image = HeldImage::open(native_path, CANDIDATE_SHA256)?;
        verify(&image)?;
        Ok(Self(image))
    }
    pub(super) fn held(&self) -> &HeldImage {
        &self.0
    }
}

struct TrustState(WINTRUST_DATA);
impl Drop for TrustState {
    fn drop(&mut self) {
        self.0.dwStateAction = WTD_STATEACTION_CLOSE;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: closes only state owned by the corresponding verification call.
        unsafe { WinVerifyTrust(HWND::default(), &mut action, (&raw mut self.0).cast()) };
    }
}
pub(super) fn verify(image: &HeldImage) -> Result<(), &'static str> {
    let path = wide(image.path.as_os_str())?;
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path.as_ptr()),
        hFile: HANDLE(image.file.as_raw_handle()),
        ..Default::default()
    };
    let mut state = TrustState(WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | WTD_DISABLE_MD2_MD4,
        ..Default::default()
    });
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: file/path/state are live through verification and state cleanup;
    // held image and ancestors exclude write/delete replacement throughout.
    let trust = unsafe { WinVerifyTrust(HWND::default(), &mut action, (&raw mut state.0).cast()) };
    if trust != 0 {
        super::diagnostic("vendor-authenticode", trust);
        return Err("vendor Authenticode chain or revocation verification failed");
    }
    let provider = unsafe { WTHelperProvDataFromStateData(state.0.hWVTStateData) };
    if provider.is_null() {
        return Err("vendor signing provider unavailable");
    }
    let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, false, 0) };
    if signer.is_null() {
        return Err("vendor signer unavailable");
    }
    let signer = unsafe { &*signer };
    if signer.dwError != 0 || signer.csCertChain == 0 || signer.pasCertChain.is_null() {
        return Err("vendor signing chain invalid");
    }
    let certificate = unsafe { &*signer.pasCertChain };
    if certificate.pCert.is_null() || certificate.dwError != 0 {
        return Err("vendor signing certificate invalid");
    }
    let mut publisher = [0u16; 256];
    let len = unsafe {
        CertGetNameStringW(
            certificate.pCert,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            None,
            Some(&mut publisher),
        )
    } as usize;
    if len == 0
        || len > publisher.len()
        || String::from_utf16(&publisher[..len - 1]).ok().as_deref() != Some("Anthropic, PBC")
    {
        return Err("vendor Authenticode publisher mismatch");
    }
    Ok(())
}
