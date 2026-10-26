//! Debug-only synthetic store custody. No production path or native account adapter.
//!
//! Receipts are deliberately in-process capabilities, not restart provenance. A
//! persistent dirty byte blocks reuse even if a crashed process loses its lock.
//! No store destructor clears that byte or deletes a store directory. A clean
//! last Profile Arc still runs the existing fixture Profile destructor; unresolved
//! custody deliberately retains it. A process crash can leave fixture residue.
#![cfg(all(debug_assertions, feature = "native-fixtures"))]

use super::security::Profile;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

mod native;
pub(super) mod process_fixtures;
use native::{HeldDirectory, Identity, Policy};

const LOCK: &str = "custody.lock";
const STATE: &str = "custody.state";
const CONFIG: &str = "official-config";
const CLEAN: &[u8; 9] = b"ZCLST001\0";
const DIRTY: &[u8; 9] = b"ZCLST001\x01";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Busy,
    Collision,
    Dirty,
    Invalid,
    Io,
    Quarantined,
}

/// These names are synthetic domains, never product/trial root selectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FixtureDomain {
    ProductModel,
    TrialModel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FixtureKey {
    pub(super) domain: FixtureDomain,
    pub(super) profile: u128,
    pub(super) incarnation: u128,
}

#[derive(Clone, Copy)]
enum FixturePayload {
    Data,
    Replace,
}
impl FixturePayload {
    fn policy(self, owner: &str, package: &str) -> Policy {
        match self {
            Self::Data => Policy::directory(owner, package, true),
            Self::Replace => Policy::replace_directory(owner, package),
        }
    }
}

struct Receipt {
    parent: PathBuf,
    root_name: String,
    key: FixtureKey,
    payload: FixturePayload,
    chain: Vec<native::DirectoryReceipt>,
    config: Identity,
    lock: Identity,
    state: Identity,
    // Keeps the existing fresh fixture identity alive through every borrower.
    profile: Arc<Profile>,
    owner_sid: String,
    package_sid: String,
}

/// Not serializable, reconstructible from a path, or transferable to another run.
#[derive(Clone)]
pub(super) struct InProcessFixtureReceipt(Arc<Receipt>);

/// Owns a fresh synthetic receipt; no store directory cleanup is performed.
/// Last clean ownership release can run the existing fixture Profile destructor.
pub(super) struct FixtureStoreOwner {
    receipt: InProcessFixtureReceipt,
}

pub(super) struct StoreLease {
    inner: Arc<Inner>,
}
struct Inner {
    receipt: InProcessFixtureReceipt,
    chain: Vec<HeldDirectory>,
    config: HeldDirectory,
    lock: File,
    state: Mutex<File>,
    active: Mutex<bool>,
    quarantined: AtomicBool,
}

// Fixed stage names and closed numeric categories only. This never reports a
// path, SID, descriptor, account value or native caller-provided text.
fn stage<T>(name: &'static str, result: Result<T, Error>) -> Result<T, Error> {
    let code = match &result {
        Ok(_) => 0,
        Err(Error::Busy) => 1,
        Err(Error::Collision) => 2,
        Err(Error::Dirty) => 3,
        Err(Error::Invalid) => 4,
        Err(Error::Io) => 5,
        Err(Error::Quarantined) => 6,
    };
    super::diagnostic(name, code);
    result
}

impl FixtureStoreOwner {
    /// Creates exactly one new random synthetic child of the held temp chain.
    /// No retry/adoption/fallback follows a collision or ambiguous publication.
    pub(super) fn create(profile: Profile, key: FixtureKey) -> Result<(Self, StoreLease), Error> {
        Self::create_with_payload(profile, key, FixturePayload::Data)
    }
    /// Fresh synthetic comparison only. No existing store or ordinary caller is
    /// upgraded; the immutable in-process receipt binds this exact policy.
    pub(super) fn create_replace_fixture(
        profile: Profile,
        key: FixtureKey,
    ) -> Result<(Self, StoreLease), Error> {
        Self::create_with_payload(profile, key, FixturePayload::Replace)
    }
    fn create_with_payload(
        profile: Profile,
        key: FixtureKey,
        payload: FixturePayload,
    ) -> Result<(Self, StoreLease), Error> {
        // Consume the non-Clone fresh Profile. Two fixture keys cannot share a
        // package principal by passing the same Arc into separate store roots.
        let profile = Arc::new(profile);
        let (owner_sid, package_sid) = stage(
            "store-create-principals",
            profile.fixture_sids().map_err(|_| Error::Invalid),
        )?;
        let mut chain = stage("store-create-parent", native::pin_temp_parent(&owner_sid))?;
        // Only a fully rebound strict canonical chain reaches fresh creation.
        let parent = stage(
            "store-create-canonical-parent",
            native::canonical_parent(&chain.last().ok_or(Error::Invalid)?.file),
        )?;
        let root_name = stage("store-create-random-name", native::fresh_name())?;
        let root_policy = Policy::directory(&owner_sid, &package_sid, false);
        let root = stage(
            "store-create-root",
            native::create_directory(
                &chain.last().ok_or(Error::Invalid)?.file,
                &root_name,
                &root_policy,
            ),
        )?;
        stage(
            "store-create-parent-sync",
            native::sync_directory(&chain.last().ok_or(Error::Invalid)?.file),
        )?;
        chain.push(root);
        let root = &chain.last().ok_or(Error::Invalid)?.file;
        let broker_policy = Policy::broker_file(&owner_sid);
        let lock = stage(
            "store-create-lock",
            native::create_file(root, LOCK, &broker_policy),
        )?;
        stage("store-create-lease", native::lock_exclusive(&lock))?;
        let mut state = stage(
            "store-create-state",
            native::create_file(root, STATE, &broker_policy),
        )?;
        // Partial fresh creation is never represented as a clean reusable store.
        stage(
            "store-create-dirty-write",
            state.write_all(DIRTY).map_err(|_| Error::Io),
        )?;
        stage(
            "store-create-dirty-sync",
            state.sync_all().map_err(|_| Error::Io),
        )?;
        stage(
            "store-create-state-parent-sync",
            native::sync_directory(root),
        )?;
        let config = stage(
            "store-create-config",
            native::create_directory(root, CONFIG, &payload.policy(&owner_sid, &package_sid)),
        )?;
        stage(
            "store-create-config-parent-sync",
            native::sync_directory(root),
        )?;
        let receipt = InProcessFixtureReceipt(Arc::new(Receipt {
            parent,
            root_name,
            key,
            payload,
            chain: chain.iter().map(HeldDirectory::receipt).collect(),
            config: config.identity,
            lock: native::identity(&lock, false)?,
            state: native::identity(&state, false)?,
            profile,
            owner_sid,
            package_sid,
        }));
        let inner = Arc::new(Inner {
            receipt: receipt.clone(),
            chain,
            config,
            lock,
            state: Mutex::new(state),
            active: Mutex::new(false),
            quarantined: AtomicBool::new(false),
        });
        stage("store-create-final-proof", inner.verify())?;
        // No process has been admitted by this fresh-only constructor.
        stage("store-create-clean-sync", inner.write_state(CLEAN))?;
        Ok((Self { receipt }, StoreLease { inner }))
    }
    pub(super) fn receipt(&self) -> InProcessFixtureReceipt {
        self.receipt.clone()
    }
}

impl StoreLease {
    /// Read-only failed-operation observation; never grants cleanup authority.
    pub(super) fn fixture_failed_dirty(&self) -> Result<(), Error> {
        if !self.inner.quarantined.load(Ordering::Acquire)
            || !*self.inner.active.lock().map_err(|_| Error::Quarantined)?
        {
            return Err(Error::Invalid);
        }
        self.inner.require_state(DIRTY)
    }
    /// Exact in-process reopen only. Missing nodes and dirty state are not repaired.
    pub(super) fn reopen(
        receipt: &InProcessFixtureReceipt,
        key: FixtureKey,
    ) -> Result<Self, Error> {
        let expected = &receipt.0;
        if key != expected.key {
            return Err(Error::Invalid);
        }
        let sids = expected
            .profile
            .fixture_sids()
            .map_err(|_| Error::Invalid)?;
        if sids != (expected.owner_sid.clone(), expected.package_sid.clone()) {
            return Err(Error::Invalid);
        }
        let mut chain = native::pin_parent(&expected.parent, &expected.owner_sid)?;
        let root = native::open_directory(
            &chain.last().ok_or(Error::Invalid)?.file,
            &expected.root_name,
            &Policy::directory(&expected.owner_sid, &expected.package_sid, false),
        )?;
        chain.push(root);
        if chain.iter().map(HeldDirectory::receipt).collect::<Vec<_>>() != expected.chain {
            return Err(Error::Invalid);
        }
        let parent = &chain.last().ok_or(Error::Invalid)?.file;
        let broker_policy = Policy::broker_file(&expected.owner_sid);
        let lock = native::open_file(parent, LOCK, &broker_policy)?;
        if native::identity(&lock, false)? != expected.lock
            || lock.metadata().map_err(|_| Error::Io)?.len() != 0
        {
            return Err(Error::Invalid);
        }
        native::lock_exclusive(&lock)?;
        // Mutable state is inspected only while holding the original lease.
        let state = native::open_file(parent, STATE, &broker_policy)?;
        let config = native::open_directory(
            parent,
            CONFIG,
            &expected
                .payload
                .policy(&expected.owner_sid, &expected.package_sid),
        )?;
        let inner = Arc::new(Inner {
            receipt: receipt.clone(),
            chain,
            config,
            lock,
            state: Mutex::new(state),
            active: Mutex::new(false),
            quarantined: AtomicBool::new(false),
        });
        inner.verify()?;
        inner.require_clean()?;
        Ok(Self { inner })
    }
    pub(super) fn verify(&self) -> Result<(), Error> {
        self.inner.verify()
    }

    /// Retains lifetime custody and flushes dirty state before any future spawn.
    pub(super) fn begin_operation(&self) -> Result<ActiveStore, Error> {
        let mut active = self.inner.active.lock().map_err(|_| {
            self.inner.quarantined.store(true, Ordering::Release);
            Error::Quarantined
        })?;
        if *active {
            return Err(Error::Busy);
        }
        self.inner.verify()?;
        self.inner.require_clean()?;
        self.inner.write_state(DIRTY)?;
        *active = true;
        Ok(ActiveStore {
            inner: Arc::clone(&self.inner),
            settled: false,
        })
    }
}

/// Only this opaque type can clear an active record. There is no production or
/// original-client constructor: that owning-worker adapter is not integrated.
/// The fixed debug synthetic runner can construct it only after native settlement;
/// no renderer, general Port or boolean can grant this authority.
pub(super) struct WorkerSettled {
    _private: (),
}

/// Opaque lifetime extension, not an extractable Arc or new-profile authority.
pub(super) struct FixtureProfile(Arc<Profile>);
impl std::ops::Deref for FixtureProfile {
    type Target = Profile;
    fn deref(&self) -> &Profile {
        &self.0
    }
}

/// Independently retains every lease/ancestor even after all views/owners drop.
pub(super) struct ActiveStore {
    inner: Arc<Inner>,
    settled: bool,
}
impl ActiveStore {
    /// Same retained principal only; Profile itself cannot be cloned/adopted into
    /// a new store. This call borrow survives movement into the native guard.
    pub(super) fn fixture_profile(&self) -> Result<FixtureProfile, Error> {
        self.inner.verify()?;
        Ok(FixtureProfile(Arc::clone(&self.inner.receipt.0.profile)))
    }
    pub(super) fn fixture_key(&self) -> FixtureKey {
        self.inner.receipt.0.key
    }
    /// Only the fixed native return carrying its OWN active operation can clean.
    /// Neither a caller boolean nor a separate owner/token is accepted.
    pub(super) fn finish_config_fixture(
        call: super::process::SettledStoreCall,
    ) -> Result<(), &'static str> {
        use std::{io::Read, os::windows::fs::OpenOptionsExt};
        let (active, mut output, operation) = call.into_parts();
        let stdout = zeroize::Zeroizing::new(std::mem::take(&mut output.stdout));
        let stderr = zeroize::Zeroizing::new(std::mem::take(&mut output.stderr));
        if let Err(failure) =
            super::store_session::validate(output.code, output.stopped.is_some(), &stdout, &stderr)
        {
            super::diagnostic("store-normal-config-failed-stage", failure.stage);
            super::diagnostic("store-normal-config-failed-code", failure.code);
            // Drop retains DIRTY/quarantined custody, including compatibility
            // errors. Exit alone or partial ordinary writes never mint CLEAN.
            return Err("normal config process contract failed");
        }
        let config = active
            .config_path()
            .map_err(|_| "normal config owner proof failed")?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(0x0020_0000)
            .open(config.join("synthetic-settings.json"))
            .map_err(|_| "normal config payload unavailable")?;
        native::identity(&file, false).map_err(|_| "normal config payload identity rejected")?;
        let mut bytes = Vec::new();
        file.take(128)
            .read_to_end(&mut bytes)
            .map_err(|_| "normal config payload read failed")?;
        if bytes != operation.expected() {
            return Err("normal config payload mismatch");
        }
        match std::fs::symlink_metadata(config.join(".synthetic-settings.tmp")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("normal config temporary was not replaced"),
        }
        active
            .settle(WorkerSettled { _private: () })
            .map_err(|_| "normal config clean settlement failed")
    }
    /// Path observation is available only under active, independently held custody.
    pub(super) fn config_path(&self) -> Result<PathBuf, Error> {
        self.inner.verify()?;
        if let Err(error) = self.inner.require_state(DIRTY) {
            self.inner.quarantined.store(true, Ordering::Release);
            return Err(error);
        }
        let receipt = &self.inner.receipt.0;
        Ok(receipt.parent.join(&receipt.root_name).join(CONFIG))
    }
    pub(super) fn settle(mut self, _proof: WorkerSettled) -> Result<(), Error> {
        let mut active = self.inner.active.lock().map_err(|_| Error::Quarantined)?;
        if !*active {
            return Err(Error::Invalid);
        }
        self.inner.verify()?;
        self.inner.require_state(DIRTY)?;
        self.inner.write_state(CLEAN)?;
        *active = false;
        self.settled = true;
        Ok(())
    }
}
impl Drop for ActiveStore {
    fn drop(&mut self) {
        if !self.settled {
            self.inner.quarantined.store(true, Ordering::Release);
            // An unresolved worker must not release the native lease on panic or
            // UI abandonment. Deliberately retain until process teardown; the
            // persisted dirty record then blocks any subsequent opener.
            std::mem::forget(Arc::clone(&self.inner));
        }
    }
}

impl Inner {
    fn verify(&self) -> Result<(), Error> {
        if self.quarantined.load(Ordering::Acquire) {
            return Err(Error::Quarantined);
        }
        let result = self.verify_inner();
        if result.is_err() {
            self.quarantined.store(true, Ordering::Release);
        }
        result
    }
    fn verify_inner(&self) -> Result<(), Error> {
        let receipt = &self.receipt.0;
        native::verify_chain(&self.chain, &receipt.chain, &receipt.owner_sid)?;
        let parent = &self.chain.last().ok_or(Error::Invalid)?.file;
        native::verify_directory(
            parent,
            CONFIG,
            &self.config,
            &receipt
                .payload
                .policy(&receipt.owner_sid, &receipt.package_sid),
        )?;
        if self.config.identity != receipt.config {
            return Err(Error::Invalid);
        }
        let policy = Policy::broker_file(&receipt.owner_sid);
        native::verify_file(parent, LOCK, &self.lock, receipt.lock, &policy)?;
        if self.lock.metadata().map_err(|_| Error::Io)?.len() != 0 {
            return Err(Error::Invalid);
        }
        let state = self.state.lock().map_err(|_| Error::Quarantined)?;
        native::verify_file(parent, STATE, &state, receipt.state, &policy)?;
        if state.metadata().map_err(|_| Error::Io)?.len() != CLEAN.len() as u64 {
            return Err(Error::Dirty);
        }
        Ok(())
    }
    fn require_clean(&self) -> Result<(), Error> {
        self.require_state(CLEAN)
    }
    fn require_state(&self, expected: &[u8; 9]) -> Result<(), Error> {
        // File clones share the kernel cursor: always serialize all marker I/O,
        // including construction/reopen proof, independently of operation state.
        let guard = self.state.lock().map_err(|_| {
            self.quarantined.store(true, Ordering::Release);
            Error::Quarantined
        })?;
        require_file_state(&guard, expected)
    }

    fn write_state(&self, state: &[u8; 9]) -> Result<(), Error> {
        let result = (|| {
            let guard = self.state.lock().map_err(|_| Error::Quarantined)?;
            write_file_state(&guard, state)
        })();
        if result.is_err() {
            self.quarantined.store(true, Ordering::Release);
        }
        result
    }
}

// Callers must own exclusive store custody and serialize the shared file cursor.
// The fixed cooperating-process fixture uses these same exact marker operations.
fn require_file_state(mut file: &File, expected: &[u8; 9]) -> Result<(), Error> {
    if file.metadata().map_err(|_| Error::Io)?.len() != expected.len() as u64 {
        return Err(Error::Dirty);
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| Error::Io)?;
    let mut state = [0; CLEAN.len()];
    file.read_exact(&mut state).map_err(|_| Error::Io)?;
    if &state != expected {
        return Err(Error::Dirty);
    }
    Ok(())
}

fn write_file_state(mut file: &File, state: &[u8; 9]) -> Result<(), Error> {
    if file.metadata().map_err(|_| Error::Io)?.len() != state.len() as u64 {
        return Err(Error::Dirty);
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| Error::Io)?;
    file.write_all(state).map_err(|_| Error::Io)?;
    file.sync_all().map_err(|_| Error::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests;
