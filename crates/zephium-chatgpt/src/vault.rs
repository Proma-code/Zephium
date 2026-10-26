//! Windows Credential Manager storage, separate from API keys and CLI credentials.
//! A generation's manifest is published last, so a partial rotation preserves the
//! previous complete record. Publication succeeds even if removing old chunks
//! fails: callers must adopt the committed rotation and report pending cleanup.
//! No plaintext-file fallback exists.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::token::{valid_client, valid_host, valid_identity, Identity};

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
const CHUNK: usize = 2000;
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
const MAX_BYTES: usize = 128 * 1024;

/// Closed native storage errors, without OS details or credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultError {
    /// The protected record is absent.
    Missing,
    /// Storage is inaccessible or unavailable on this platform.
    Inaccessible,
    /// The namespace, profile, manifest or stored record is malformed.
    Invalid,
}

/// An issued registration observed in a state-validated native OAuth callback.
///
/// This is retry metadata, **not** a verified identity, authorization or account
/// status. It must never originate in frame-supplied fields or imported tokens.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRegistration {
    /// The issued client identifier; never the bootstrap client identifier.
    pub client_id: String,
    /// The app installation's stable UUIDv4 host identifier.
    pub host_id: String,
}

impl PendingRegistration {
    /// Validate callback-issued metadata without claiming a verified account.
    pub fn new(host_id: &str, client_id: &str) -> Result<Self, VaultError> {
        let pending = Self {
            client_id: client_id.into(),
            host_id: host_id.into(),
        };
        if !valid_pending(&pending) {
            return Err(VaultError::Invalid);
        }
        Ok(pending)
    }
}

/// Protected registration metadata and optional secret-bearing token bytes.
///
/// An initial failed exchange can leave only `pending_registration`. Such a
/// record cannot contain tokens or project an authenticated identity.
pub struct Stored {
    /// Cryptographically validated mapping, retained when signing out.
    pub identity: Option<Identity>,
    /// Unverified callback-issued registration retained for a fresh PKCE retry.
    pub pending_registration: Option<PendingRegistration>,
    /// Token record encoded by `Tokens::to_bytes`, never imported from the frame.
    pub tokens: Option<Zeroizing<Vec<u8>>>,
}

/// A complete new record has been committed and must replace the old record.
#[must_use = "adopt the committed record and report incomplete cleanup"]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveOutcome {
    /// At least one obsolete credential could not be removed or enumerated.
    /// On logout the active record is signed out, but local deletion is not yet
    /// complete. A later save retries cleanup within this exact namespace/profile.
    pub cleanup_pending: bool,
}

/// Per-profile storage port. Tests use memory; production uses Credential Manager.
pub trait AccountVault: Send + Sync {
    /// Read exactly the selected profile's registration.
    fn load(&self, profile: &str) -> Result<Stored, VaultError>;
    /// Atomically publish a complete rotation, registration retry, or sign-out.
    ///
    /// `Err` means the new manifest was not published. Every `Ok` means it was
    /// published, even when obsolete chunks could not subsequently be removed.
    fn save(&self, profile: &str, record: &Stored) -> Result<SaveOutcome, VaultError>;
    /// Erase this profile's registration and every current or obsolete chunk.
    ///
    /// The manifest is removed first. All remaining owned chunks are attempted
    /// even after a deletion failure. `Err` means erasure is incomplete and must
    /// be retried before acknowledging profile deletion; absence is success.
    /// The caller must retire and serialize all writers for this profile first.
    fn forget(&self, profile: &str) -> Result<(), VaultError>;
}

/// Windows-only protected storage bound to a trusted build-time application ID.
/// Other platforms fail closed; no default silently chooses a production vault.
pub struct SystemVault {
    #[cfg_attr(not(any(test, windows)), allow(dead_code))]
    namespace: &'static str,
}

impl SystemVault {
    /// Bind storage to a fixed native application identifier, never IPC input.
    ///
    /// For example the isolated trial uses `app.zephium.subscription-trial`,
    /// while the ordinary application uses `app.zephium`. The account suffix
    /// and profile delimiters are appended internally, not supplied by callers.
    pub fn new(namespace: &'static str) -> Result<Self, VaultError> {
        if !valid_namespace(namespace) {
            return Err(VaultError::Invalid);
        }
        Ok(Self { namespace })
    }
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    identity: Option<Identity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_registration: Option<PendingRegistration>,
    generation: String,
    size: usize,
    digest: [u8; 32],
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
trait Store {
    fn read(&self, key: &str) -> Result<Zeroizing<Vec<u8>>, VaultError>;
    /// On failure the old value is unchanged; native CredWrite is one operation.
    fn write(&self, key: &str, bytes: &[u8]) -> Result<(), VaultError>;
    fn delete(&self, key: &str) -> Result<(), VaultError>;
    fn list(&self, prefix: &str) -> Result<Vec<String>, VaultError>;
}

fn valid_pending(pending: &PendingRegistration) -> bool {
    valid_client(&pending.client_id) && valid_host(&pending.host_id)
}

fn valid_namespace(namespace: &str) -> bool {
    (namespace == "app.zephium" || namespace.starts_with("app.zephium."))
        && namespace.len() <= 160
        && namespace.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !part.starts_with('-')
                && !part.ends_with('-')
        })
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn valid_metadata(
    identity: Option<&Identity>,
    pending: Option<&PendingRegistration>,
    has_tokens: bool,
) -> bool {
    if identity.is_none() && (pending.is_none() || has_tokens) {
        return false;
    }
    if identity.is_some_and(|id| !valid_identity(id))
        || pending.is_some_and(|value| !valid_pending(value))
    {
        return false;
    }
    match (identity, pending) {
        (Some(id), Some(value)) => id.host_id == value.host_id,
        _ => true,
    }
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn prefix(namespace: &str, profile: &str) -> Result<String, VaultError> {
    if !valid_namespace(namespace) {
        return Err(VaultError::Invalid);
    }
    let Some(id) = zephium_core::ids::ProfileId::parse(profile) else {
        return Err(VaultError::Invalid);
    };
    if id.to_string() != profile {
        return Err(VaultError::Invalid);
    }
    Ok(format!("{namespace}.chatgpt|{profile}|"))
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn manifest(bytes: &[u8]) -> Result<Manifest, VaultError> {
    if bytes.len() > CHUNK {
        return Err(VaultError::Invalid);
    }
    let value: Manifest = serde_json::from_slice(bytes).map_err(|_| VaultError::Invalid)?;
    // Version 1 had a required Identity and no pending registration. Retain
    // compatibility with those already protected records without rewriting them.
    if !matches!(value.version, 1 | 2)
        || (value.version == 1
            && (value.identity.is_none() || value.pending_registration.is_some()))
        || !valid_metadata(
            value.identity.as_ref(),
            value.pending_registration.as_ref(),
            value.size > 0,
        )
        || value.size > MAX_BYTES
        || value.generation.len() != 32
        || !value
            .generation
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || (value.size == 0 && value.digest != <[u8; 32]>::from(Sha256::digest([])))
    {
        return Err(VaultError::Invalid);
    }
    Ok(value)
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn read(store: &impl Store, prefix: &str) -> Result<Stored, VaultError> {
    let manifest = manifest(&store.read(&format!("{prefix}manifest"))?)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(manifest.size));
    for index in 0..manifest.size.div_ceil(CHUNK) {
        let chunk = store.read(&format!("{prefix}{}{index:03}", manifest.generation))?;
        let remaining = manifest.size - bytes.len();
        if chunk.len() != remaining.min(CHUNK) {
            return Err(VaultError::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    if Sha256::digest(&bytes).as_slice() != manifest.digest {
        return Err(VaultError::Invalid);
    }
    Ok(Stored {
        identity: manifest.identity,
        pending_registration: manifest.pending_registration,
        tokens: (manifest.size > 0).then_some(bytes),
    })
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn cleanup(store: &impl Store, prefix: &str, keep: &Manifest) -> bool {
    let Ok(keys) = store.list(prefix) else {
        return true;
    };
    let manifest_key = format!("{prefix}manifest");
    let keep_prefix = format!("{prefix}{}", keep.generation);
    let mut pending = false;
    for key in keys {
        // Even a malformed inventory must not expand deletion outside its owner.
        if !key.starts_with(prefix) {
            pending = true;
            continue;
        }
        if key != manifest_key && !(keep.size > 0 && key.starts_with(&keep_prefix)) {
            // Attempt every obsolete chunk; one failure must not strand others.
            pending |= store.delete(&key).is_err();
        }
    }
    pending
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn forget(store: &impl Store, prefix: &str) -> Result<(), VaultError> {
    let manifest_key = format!("{prefix}manifest");
    let mut failure = match store.delete(&manifest_key) {
        Ok(()) | Err(VaultError::Missing) => None,
        Err(error) => Some(error),
    };
    // Retire the discoverable account before enumerating secret-bearing chunks.
    // Do not require a valid manifest: malformed/partial records also need erasure.
    let keys = store.list(prefix)?;
    for key in keys {
        if !key.starts_with(prefix) {
            failure.get_or_insert(VaultError::Invalid);
            continue;
        }
        if key == manifest_key {
            continue;
        }
        match store.delete(&key) {
            Ok(()) | Err(VaultError::Missing) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
fn save(store: &impl Store, prefix: &str, record: &Stored) -> Result<SaveOutcome, VaultError> {
    if !valid_metadata(
        record.identity.as_ref(),
        record.pending_registration.as_ref(),
        record.tokens.is_some(),
    ) {
        return Err(VaultError::Invalid);
    }
    let bytes = record
        .tokens
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if bytes.len() > MAX_BYTES || (record.tokens.is_some() && bytes.is_empty()) {
        return Err(VaultError::Invalid);
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| VaultError::Inaccessible)?;
    let generation = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let current = Manifest {
        version: 2,
        identity: record.identity.clone(),
        pending_registration: record.pending_registration.clone(),
        generation,
        size: bytes.len(),
        digest: Sha256::digest(bytes).into(),
    };
    let encoded = serde_json::to_vec(&current).map_err(|_| VaultError::Invalid)?;
    if encoded.len() > CHUNK {
        return Err(VaultError::Invalid);
    }
    let mut written: Vec<String> = Vec::new();
    for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
        let key = format!("{prefix}{}{index:03}", current.generation);
        if let Err(error) = store.write(&key, chunk) {
            for key in written {
                let _ = store.delete(&key);
            }
            return Err(error);
        }
        written.push(key);
    }
    if let Err(error) = store.write(&format!("{prefix}manifest"), &encoded) {
        for key in written {
            let _ = store.delete(&key);
        }
        return Err(error);
    }
    // This is the publication boundary. Never return Err after this point: a
    // caller keeping the old rotating refresh token would now use stale state.
    Ok(SaveOutcome {
        cleanup_pending: cleanup(store, prefix, &current),
    })
}

#[cfg(windows)]
mod native {
    use super::*;
    struct Native;
    fn fault(error: zephium_credentials::VaultError) -> VaultError {
        if error == zephium_credentials::VaultError::Missing {
            VaultError::Missing
        } else {
            VaultError::Inaccessible
        }
    }
    impl Store for Native {
        fn read(&self, key: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
            zephium_credentials::read(key).map_err(fault)
        }
        fn write(&self, key: &str, bytes: &[u8]) -> Result<(), VaultError> {
            zephium_credentials::write(key, bytes).map_err(fault)
        }
        fn delete(&self, key: &str) -> Result<(), VaultError> {
            match zephium_credentials::delete(key) {
                Err(zephium_credentials::VaultError::Missing) => Ok(()),
                result => result.map_err(fault),
            }
        }
        fn list(&self, prefix: &str) -> Result<Vec<String>, VaultError> {
            zephium_credentials::list(prefix).map_err(fault)
        }
    }
    impl AccountVault for SystemVault {
        fn load(&self, profile: &str) -> Result<Stored, VaultError> {
            let prefix = prefix(self.namespace, profile)?;
            let _turn = zephium_credentials::turn().map_err(fault)?;
            read(&Native, &prefix)
        }
        fn save(&self, profile: &str, record: &Stored) -> Result<SaveOutcome, VaultError> {
            let prefix = prefix(self.namespace, profile)?;
            let _turn = zephium_credentials::turn().map_err(fault)?;
            save(&Native, &prefix, record)
        }
        fn forget(&self, profile: &str) -> Result<(), VaultError> {
            let prefix = prefix(self.namespace, profile)?;
            let _turn = zephium_credentials::turn().map_err(fault)?;
            forget(&Native, &prefix)
        }
    }
}

#[cfg(not(windows))]
impl AccountVault for SystemVault {
    fn load(&self, _: &str) -> Result<Stored, VaultError> {
        Err(VaultError::Inaccessible)
    }
    fn save(&self, _: &str, _: &Stored) -> Result<SaveOutcome, VaultError> {
        Err(VaultError::Inaccessible)
    }
    fn forget(&self, _: &str) -> Result<(), VaultError> {
        Err(VaultError::Inaccessible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    const PROFILE: &str = "00000000000000000000000001";
    const HOST: &str = "urn:uuid:12345678-1234-4123-8123-123456789abc";
    #[derive(Default)]
    struct Memory {
        values: RefCell<BTreeMap<String, Vec<u8>>>,
        fail_write: RefCell<Option<String>>,
        fail_delete: RefCell<Option<String>>,
        fail_list: Cell<bool>,
        delete_attempts: RefCell<Vec<String>>,
    }
    impl Store for Memory {
        fn read(&self, key: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
            self.values
                .borrow()
                .get(key)
                .cloned()
                .map(Zeroizing::new)
                .ok_or(VaultError::Missing)
        }
        fn write(&self, key: &str, bytes: &[u8]) -> Result<(), VaultError> {
            if self
                .fail_write
                .borrow()
                .as_ref()
                .is_some_and(|suffix| key.ends_with(suffix))
            {
                return Err(VaultError::Inaccessible);
            }
            self.values.borrow_mut().insert(key.into(), bytes.to_vec());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<(), VaultError> {
            self.delete_attempts.borrow_mut().push(key.into());
            if self
                .fail_delete
                .borrow()
                .as_ref()
                .is_some_and(|suffix| key.ends_with(suffix))
            {
                return Err(VaultError::Inaccessible);
            }
            self.values.borrow_mut().remove(key);
            Ok(())
        }
        fn list(&self, prefix: &str) -> Result<Vec<String>, VaultError> {
            if self.fail_list.get() {
                return Err(VaultError::Inaccessible);
            }
            Ok(self
                .values
                .borrow()
                .keys()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect())
        }
    }
    fn record(bytes: usize) -> Stored {
        Stored {
            identity: Some(Identity {
                issuer: "https://auth.openai.com".into(),
                subject: "user".into(),
                client_id: "oaiapp_test".into(),
                host_id: HOST.into(),
                email: None,
            }),
            pending_registration: None,
            tokens: (bytes > 0).then(|| Zeroizing::new(vec![b'x'; bytes])),
        }
    }
    fn fixture_prefix() -> String {
        prefix("app.zephium.subscription-trial", PROFILE).unwrap()
    }

    #[test]
    fn forgetting_retires_manifest_first_and_isolates_namespace_and_profile() {
        let store = Memory::default();
        let own = fixture_prefix();
        let production = prefix("app.zephium", PROFILE).unwrap();
        let other_profile = prefix(
            "app.zephium.subscription-trial",
            "00000000000000000000000002",
        )
        .unwrap();
        for prefix in [&own, &production, &other_profile] {
            let _ = save(&store, prefix, &record(6000)).unwrap();
        }
        // Orphaned chunks from an interrupted rotation must also be erased.
        store
            .write(&format!("{own}orphaned-generation000"), b"synthetic")
            .unwrap();
        store.delete_attempts.borrow_mut().clear();
        assert_eq!(forget(&store, &own), Ok(()));
        assert_eq!(store.delete_attempts.borrow()[0], format!("{own}manifest"));
        assert!(!store
            .values
            .borrow()
            .keys()
            .any(|key| key.starts_with(&own)));
        assert!(matches!(read(&store, &own), Err(VaultError::Missing)));
        for prefix in [&production, &other_profile] {
            assert_eq!(read(&store, prefix).unwrap().tokens.unwrap().len(), 6000);
        }
        assert_eq!(forget(&store, &own), Ok(()), "already erased is idempotent");
    }

    #[test]
    fn forgetting_reports_partial_failure_attempts_other_chunks_and_retries() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let _ = save(&store, &prefix, &record(6000)).unwrap();
        store.delete_attempts.borrow_mut().clear();
        *store.fail_delete.borrow_mut() = Some("000".into());
        assert_eq!(forget(&store, &prefix), Err(VaultError::Inaccessible));
        assert_eq!(
            store.delete_attempts.borrow().len(),
            4,
            "manifest plus every chunk attempted"
        );
        assert_eq!(store.values.borrow().len(), 1, "only failed chunk remains");
        assert!(matches!(read(&store, &prefix), Err(VaultError::Missing)));
        *store.fail_delete.borrow_mut() = None;
        assert_eq!(forget(&store, &prefix), Ok(()));
        assert!(store.values.borrow().is_empty());
    }

    #[test]
    fn forgetting_reports_manifest_and_inventory_failures_without_false_success() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let _ = save(&store, &prefix, &record(6000)).unwrap();
        *store.fail_delete.borrow_mut() = Some("manifest".into());
        assert_eq!(forget(&store, &prefix), Err(VaultError::Inaccessible));
        assert_eq!(
            store.values.borrow().len(),
            1,
            "chunks deleted despite manifest failure"
        );
        *store.fail_delete.borrow_mut() = None;
        assert_eq!(forget(&store, &prefix), Ok(()));
        let _ = save(&store, &prefix, &record(6000)).unwrap();
        store.fail_list.set(true);
        assert_eq!(forget(&store, &prefix), Err(VaultError::Inaccessible));
        assert!(matches!(read(&store, &prefix), Err(VaultError::Missing)));
        assert_eq!(
            store.values.borrow().len(),
            3,
            "enumeration failure cannot claim erasure"
        );
        store.fail_list.set(false);
        assert_eq!(forget(&store, &prefix), Ok(()));
        assert!(store.values.borrow().is_empty());
    }

    #[test]
    fn prepublication_failures_preserve_previous_record_and_signout_preserves_mapping() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let _ = save(&store, &prefix, &record(6000)).unwrap();
        for failed in ["001", "manifest"] {
            *store.fail_write.borrow_mut() = Some(failed.into());
            assert_eq!(
                save(&store, &prefix, &record(8000)),
                Err(VaultError::Inaccessible)
            );
            assert_eq!(read(&store, &prefix).unwrap().tokens.unwrap().len(), 6000);
        }
        *store.fail_write.borrow_mut() = None;
        assert_eq!(
            save(&store, &prefix, &record(0)),
            Ok(SaveOutcome {
                cleanup_pending: false
            })
        );
        let signed_out = read(&store, &prefix).unwrap();
        assert!(signed_out.tokens.is_none());
        assert_eq!(signed_out.identity.unwrap().client_id, "oaiapp_test");
        assert_eq!(store.values.borrow().len(), 1);
    }

    #[test]
    fn committed_rotation_is_adopted_even_when_old_chunk_deletion_fails() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let _ = save(&store, &prefix, &record(6000)).unwrap();
        *store.fail_delete.borrow_mut() = Some("000".into());
        let replacement = record(8000);
        let outcome = save(&store, &prefix, &replacement).expect("new manifest committed");
        assert!(outcome.cleanup_pending);
        assert_eq!(read(&store, &prefix).unwrap().tokens.unwrap().len(), 8000);
        assert_eq!(
            store.delete_attempts.borrow().len(),
            3,
            "failure does not stop other deletions"
        );
        *store.fail_delete.borrow_mut() = None;
        assert!(!save(&store, &prefix, &replacement).unwrap().cleanup_pending);
        assert_eq!(
            store.values.borrow().len(),
            5,
            "one manifest and only current chunks remain"
        );
    }

    #[test]
    fn logout_commits_but_reports_incomplete_local_deletion() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let _ = save(&store, &prefix, &record(6000)).unwrap();
        *store.fail_delete.borrow_mut() = Some("000".into());
        assert!(save(&store, &prefix, &record(0)).unwrap().cleanup_pending);
        let signed_out = read(&store, &prefix).unwrap();
        assert!(signed_out.tokens.is_none());
        assert!(signed_out.identity.is_some());
        assert_eq!(
            store.values.borrow().len(),
            2,
            "undeleted old secret is explicitly reported"
        );
        *store.fail_delete.borrow_mut() = None;
        assert!(!save(&store, &prefix, &signed_out).unwrap().cleanup_pending);
        assert_eq!(store.values.borrow().len(), 1);
    }

    #[test]
    fn inventory_failure_after_publication_is_success_with_pending_cleanup() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let _ = save(&store, &prefix, &record(6000)).unwrap();
        store.fail_list.set(true);
        assert!(
            save(&store, &prefix, &record(8000))
                .unwrap()
                .cleanup_pending
        );
        assert_eq!(read(&store, &prefix).unwrap().tokens.unwrap().len(), 8000);
    }

    #[test]
    fn pending_registration_is_not_an_identity_or_authorization() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let pending = PendingRegistration::new(HOST, "oaiapp_pending").unwrap();
        let mut registration = Stored {
            identity: None,
            pending_registration: Some(pending.clone()),
            tokens: None,
        };
        let _ = save(&store, &prefix, &registration).unwrap();
        let restored = read(&store, &prefix).unwrap();
        assert!(restored.identity.is_none());
        assert!(restored.tokens.is_none());
        assert_eq!(restored.pending_registration, Some(pending.clone()));
        registration.tokens = Some(Zeroizing::new(vec![1, 2, 3]));
        assert_eq!(
            save(&store, &prefix, &registration),
            Err(VaultError::Invalid)
        );
        let mut verified = record(6000);
        verified.pending_registration = Some(pending);
        let _ = save(&store, &prefix, &verified).unwrap();
        let restored = read(&store, &prefix).unwrap();
        assert_eq!(restored.identity.unwrap().subject, "user");
        assert!(restored.pending_registration.is_some());
        assert_eq!(restored.tokens.unwrap().len(), 6000);
        assert!(PendingRegistration::new(HOST, "dynamic_agent_client").is_err());
        verified.pending_registration.as_mut().unwrap().host_id =
            "urn:uuid:12345678-1234-4123-8123-123456789abd".into();
        assert_eq!(save(&store, &prefix, &verified), Err(VaultError::Invalid));
    }

    #[test]
    fn chunks_are_integrity_checked_and_profiles_and_namespaces_isolated() {
        let store = Memory::default();
        let one = fixture_prefix();
        let two = prefix(
            "app.zephium.subscription-trial",
            "00000000000000000000000002",
        )
        .unwrap();
        let production = prefix("app.zephium", PROFILE).unwrap();
        let _ = save(&store, &production, &record(8000)).unwrap();
        let _ = save(&store, &one, &record(6000)).unwrap();
        assert!(matches!(read(&store, &two), Err(VaultError::Missing)));
        let key = store
            .values
            .borrow()
            .keys()
            .find(|key| key.starts_with(&one) && key.ends_with("000"))
            .unwrap()
            .clone();
        store.values.borrow_mut().get_mut(&key).unwrap()[0] = b'y';
        assert!(matches!(read(&store, &one), Err(VaultError::Invalid)));
        let _ = save(&store, &one, &record(0)).unwrap();
        assert_eq!(
            read(&store, &production).unwrap().tokens.unwrap().len(),
            8000
        );
        assert!(prefix("app.zephium", "../other").is_err());
        assert_eq!(
            SystemVault::new("app.zephium.subscription-trial")
                .unwrap()
                .namespace,
            "app.zephium.subscription-trial"
        );
        for invalid in [
            "other.app",
            "app.zephiumevil",
            "app.zephium.*",
            "app.zephium|other",
            "app.zephium..trial",
            "app.zephium.-trial",
            "app.zephium.TRIAL",
        ] {
            assert!(SystemVault::new(invalid).is_err());
        }
    }

    #[test]
    fn legacy_verified_records_load_but_unverified_or_empty_metadata_fails() {
        let store = Memory::default();
        let prefix = fixture_prefix();
        let _ = save(&store, &prefix, &record(0)).unwrap();
        let key = format!("{prefix}manifest");
        let mut value: serde_json::Value =
            serde_json::from_slice(&store.read(&key).unwrap()).unwrap();
        value["version"] = 1.into();
        store
            .write(&key, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(read(&store, &prefix).unwrap().identity.is_some());
        value["identity"] = serde_json::Value::Null;
        store
            .write(&key, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(matches!(read(&store, &prefix), Err(VaultError::Invalid)));
        value["version"] = 2.into();
        store
            .write(&key, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(matches!(read(&store, &prefix), Err(VaultError::Invalid)));
        assert!(matches!(
            save(
                &store,
                &prefix,
                &Stored {
                    identity: None,
                    pending_registration: None,
                    tokens: None
                }
            ),
            Err(VaultError::Invalid)
        ));
    }
}
