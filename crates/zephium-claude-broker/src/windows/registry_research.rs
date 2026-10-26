//! Fresh synthetic registry rights probes. No existing application keys/values.
use super::super::{security::Profile, wide};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    mem::size_of_val,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows::{
    core::{w, PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SDDL_REVISION_1, SE_REGISTRY_KEY,
            },
            GetTokenInformation, TokenOwner, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            PSID, SECURITY_ATTRIBUTES, TOKEN_OWNER, TOKEN_QUERY,
        },
        System::{
            Registry::{
                RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW, HKEY, HKEY_USERS, KEY_ALL_ACCESS, KEY_CREATE_SUB_KEY,
                KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY,
                REG_BINARY, REG_CREATED_NEW_KEY, REG_CREATE_KEY_DISPOSITION, REG_OPTION_VOLATILE,
                REG_SAM_FLAGS, REG_VALUE_TYPE,
            },
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
};

struct Key(HKEY);
impl Key {
    fn close(mut self) -> Result<(), &'static str> {
        let raw = std::mem::take(&mut self.0);
        unsafe { RegCloseKey(raw) }
            .ok()
            .map_err(|_| "registry fixture handle close failed")
    }
}
impl Drop for Key {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}
fn sid_text(sid: PSID) -> Result<String, &'static str> {
    let mut value = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut value) }
        .map_err(|_| "registry fixture SID invalid")?;
    let result = unsafe { value.to_string() }.map_err(|_| "registry fixture SID encoding failed");
    unsafe { LocalFree(Some(HLOCAL(value.0.cast()))) };
    result
}
fn default_owner() -> Result<String, &'static str> {
    let mut raw = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }
        .map_err(|_| "registry fixture owner unavailable")?;
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut bytes = [0usize; 128];
    let mut length = 0;
    unsafe {
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenOwner,
            Some(bytes.as_mut_ptr().cast()),
            size_of_val(&bytes) as u32,
            &mut length,
        )
    }
    .map_err(|_| "registry fixture owner unavailable")?;
    if length as usize > size_of_val(&bytes)
        || (length as usize) < std::mem::size_of::<TOKEN_OWNER>()
    {
        return Err("registry fixture owner malformed");
    }
    sid_text(unsafe { (*bytes.as_ptr().cast::<TOKEN_OWNER>()).Owner })
}
fn owner_matches(key: HKEY, expected: &str) -> Result<(), &'static str> {
    let mut owner = PSID::default();
    let mut raw = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            HANDLE(key.0),
            SE_REGISTRY_KEY,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut raw),
        )
    }
    .ok()
    .map_err(|_| "registry fixture ownership query failed")?;
    let _descriptor = Descriptor(raw);
    if owner.0.is_null() || sid_text(owner)? != expected {
        return Err("registry fixture ownership mismatch");
    }
    Ok(())
}

fn marker_matches(key: HKEY, marker: &[u8]) -> Result<(), &'static str> {
    let mut bytes = [0u8; 256];
    let mut length = bytes.len() as u32;
    let mut kind = REG_VALUE_TYPE::default();
    unsafe {
        RegQueryValueExW(
            key,
            w!("synthetic-sentinel"),
            None,
            Some(&mut kind),
            Some(bytes.as_mut_ptr()),
            Some(&mut length),
        )
    }
    .ok()
    .map_err(|_| "registry fixture marker read failed")?;
    if kind != REG_BINARY || length as usize != marker.len() || &bytes[..length as usize] != marker
    {
        return Err("registry fixture marker changed");
    }
    Ok(())
}

struct Sentinel {
    key: Option<Key>,
    path: String,
    expected_owner: String,
    marker: Vec<u8>,
    cleanup_attempted: bool,
}
impl Sentinel {
    fn create(
        path: String,
        expected_owner: String,
        marker: Vec<u8>,
        descriptor: Option<&Descriptor>,
    ) -> Result<Self, &'static str> {
        let (parent_path, leaf) = path
            .rsplit_once('\\')
            .ok_or("registry fixture leaf missing")?;
        // Open the already-existing exact user Software parent first. Passing
        // only a leaf to RegCreateKeyEx forbids accidental intermediate creation.
        let parent = open(parent_path, KEY_CREATE_SUB_KEY)?
            .map_err(|_| "registry fixture parent unavailable")?;
        let name = wide(OsStr::new(leaf))?;
        let attributes = descriptor.map(|descriptor| SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0 .0,
            bInheritHandle: false.into(),
        });
        let mut raw = HKEY::default();
        let mut disposition = REG_CREATE_KEY_DISPOSITION::default();
        // RegCreateKeyEx can open an existing object. No write/cleanup authority
        // is armed unless the API confirms this exact key was newly created.
        unsafe {
            RegCreateKeyExW(
                parent.0,
                PCWSTR(name.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_VOLATILE,
                KEY_ALL_ACCESS | KEY_WOW64_64KEY,
                attributes.as_ref().map(|value| value as *const _),
                &mut raw,
                Some(&mut disposition),
            )
        }
        .ok()
        .map_err(|_| "registry fixture fresh creation failed")?;
        let key = Key(raw);
        if disposition != REG_CREATED_NEW_KEY {
            return Err("registry fixture collision rejected");
        }
        let result = Self {
            key: Some(key),
            path,
            expected_owner,
            marker,
            cleanup_attempted: false,
        };
        owner_matches(result.handle()?, &result.expected_owner)?;
        unsafe {
            RegSetValueExW(
                result.handle()?,
                w!("synthetic-sentinel"),
                None,
                REG_BINARY,
                Some(&result.marker),
            )
        }
        .ok()
        .map_err(|_| "registry fixture synthetic marker write failed")?;
        result.verify()?;
        Ok(result)
    }
    fn handle(&self) -> Result<HKEY, &'static str> {
        self.key
            .as_ref()
            .map(|key| key.0)
            .ok_or("registry fixture handle closed")
    }
    fn verify(&self) -> Result<(), &'static str> {
        owner_matches(self.handle()?, &self.expected_owner)?;
        marker_matches(self.handle()?, &self.marker)
    }
    fn cleanup(&mut self) -> Result<(), &'static str> {
        if self.cleanup_attempted {
            return Err("registry fixture cleanup already attempted");
        }
        self.verify()?;
        // Check the current name as well as the retained object before deleting.
        // This is a disposable fixture, not a persistent hostile-registry-race API.
        let named = open(&self.path, KEY_QUERY_VALUE | REG_SAM_FLAGS(0x0002_0000))?
            .map_err(|_| "registry fixture named target unavailable")?;
        owner_matches(named.0, &self.expected_owner)?;
        marker_matches(named.0, &self.marker)?;
        let name = wide(OsStr::new(&self.path))?;
        self.cleanup_attempted = true;
        // Flat key, no subkeys. No recursive deletion can touch unknown children.
        unsafe { RegDeleteKeyExW(HKEY_USERS, PCWSTR(name.as_ptr()), KEY_WOW64_64KEY.0, None) }
            .ok()
            .map_err(|_| "registry fixture deletion failed")?;
        named.close()?;
        self.key
            .take()
            .ok_or("registry fixture handle closed")?
            .close()?;
        if !matches!(open(&self.path, KEY_QUERY_VALUE)?, Err(2)) {
            return Err("registry fixture deletion absence unconfirmed");
        }
        Ok(())
    }
}
impl Drop for Sentinel {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
    }
}

fn open(path: &str, access: REG_SAM_FLAGS) -> Result<Result<Key, u32>, &'static str> {
    let name = wide(OsStr::new(path))?;
    let mut key = HKEY::default();
    let code = unsafe {
        RegOpenKeyExW(
            HKEY_USERS,
            PCWSTR(name.as_ptr()),
            None,
            access | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    Ok(if code == ERROR_SUCCESS {
        Ok(Key(key))
    } else {
        Err(code.0)
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    ordinary: String,
    foreign: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observation {
    ordinary: [u32; 4],
    foreign: [u32; 4],
}
impl Observation {
    #[cfg(test)]
    pub(super) fn denied_fixture() -> Self {
        Self {
            ordinary: [5; 4],
            foreign: [5; 4],
        }
    }
    pub(super) fn denied(&self) -> bool {
        self.ordinary == [5; 4] && self.foreign == [5; 4]
    }
    pub(super) fn report(&self) {
        for (stage, values) in [
            ("registry-ordinary-right", self.ordinary),
            ("registry-foreign-right", self.foreign),
        ] {
            for (index, value) in values.into_iter().enumerate() {
                super::super::diagnostic("registry-right-index", index as i32);
                super::super::diagnostic(stage, value as i32);
            }
        }
    }
}

pub(super) struct Sentinels {
    ordinary: Sentinel,
    foreign: Sentinel,
}
impl Sentinels {
    pub(super) fn create(profile: &Profile, suffix: &str) -> Result<Self, &'static str> {
        if suffix.is_empty()
            || !suffix
                .bytes()
                .all(|value| value.is_ascii_digit() || value == b'.')
        {
            return Err("registry fixture suffix invalid");
        }
        let (user, foreign) = profile.fixture_sids()?;
        let prefix = format!("{user}\\Software\\Zephium.ClaudeRegistryFixture.{suffix}");
        let marker = format!("synthetic-registry-only:{suffix}").into_bytes();
        if marker.len() > 128 {
            return Err("registry fixture marker exceeds bound");
        }
        // NULL security attributes preserve the ordinary Software-parent DACL;
        // the actual owner must match the parent's exact default-owner snapshot.
        let ordinary = Sentinel::create(
            format!("{prefix}.ordinary"),
            default_owner()?,
            marker.clone(),
            None,
        )?;
        let descriptor_text = wide(OsStr::new(&format!(
            "O:{user}D:P(A;;KA;;;{user})(A;;KA;;;SY)(A;;KR;;;{foreign})"
        )))?;
        let mut raw = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(descriptor_text.as_ptr()),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )
        }
        .map_err(|_| "registry foreign descriptor invalid")?;
        let descriptor = Descriptor(raw);
        let foreign =
            Sentinel::create(format!("{prefix}.foreign"), user, marker, Some(&descriptor))?;
        // Regression: opening an existing name cannot overwrite its marker or
        // arm cleanup of that already owned object.
        if !matches!(
            Sentinel::create(
                ordinary.path.clone(),
                ordinary.expected_owner.clone(),
                b"collision-must-not-write".to_vec(),
                None,
            ),
            Err("registry fixture collision rejected")
        ) {
            return Err("registry fixture collision adopted existing key");
        }
        let result = Self { ordinary, foreign };
        for sentinel in [&result.ordinary, &result.foreign] {
            for right in rights() {
                let _positive = open(&sentinel.path, right)?
                    .map_err(|_| "registry parent rights control failed")?;
            }
        }
        result.verify()?;
        Ok(result)
    }
    pub(super) fn request(&self) -> Request {
        Request {
            ordinary: self.ordinary.path.clone(),
            foreign: self.foreign.path.clone(),
        }
    }
    pub(super) fn verify(&self) -> Result<(), &'static str> {
        self.ordinary.verify()?;
        self.foreign.verify()
    }
    pub(super) fn cleanup(mut self) -> Result<(), &'static str> {
        self.ordinary.cleanup()?;
        self.foreign.cleanup()
    }
}

fn rights() -> [REG_SAM_FLAGS; 4] {
    [
        KEY_QUERY_VALUE,
        KEY_SET_VALUE,
        KEY_ENUMERATE_SUB_KEYS,
        REG_SAM_FLAGS(0x0001_0000),
    ]
}

pub(super) fn child(request: &Request) -> Result<Observation, &'static str> {
    let probe = |path: &str| -> Result<[u32; 4], &'static str> {
        let (sid, leaf) = path
            .split_once("\\Software\\")
            .ok_or("registry fixture target shape invalid")?;
        if !sid.starts_with("S-1-")
            || !sid
                .bytes()
                .all(|value| value.is_ascii_digit() || value == b'S' || value == b'-')
            || !leaf.starts_with("Zephium.ClaudeRegistryFixture.")
            || !leaf
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'.')
            || path.len() > 256
        {
            return Err("registry fixture target invalid");
        }
        let mut results = [0u32; 4];
        // Individual requested rights, not bundled KEY_READ/KEY_WRITE, and no
        // values or operations are performed by the child even if access opens.
        for (index, right) in rights().into_iter().enumerate() {
            results[index] = match open(path, right)? {
                Ok(_key) => 0,
                Err(code) => code,
            };
        }
        Ok(results)
    };
    Ok(Observation {
        ordinary: probe(&request.ordinary)?,
        foreign: probe(&request.foreign)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_read_or_any_unexpected_error_is_not_private_registry_isolation() {
        assert!(Observation {
            ordinary: [5; 4],
            foreign: [5; 4]
        }
        .denied());
        for code in [0, 2, 3, 87, 1018] {
            for index in 0..4 {
                let mut value = Observation {
                    ordinary: [5; 4],
                    foreign: [5; 4],
                };
                value.ordinary[index] = code;
                assert!(!value.denied());
                let mut value = Observation {
                    ordinary: [5; 4],
                    foreign: [5; 4],
                };
                value.foreign[index] = code;
                assert!(!value.denied());
            }
        }
    }
}
