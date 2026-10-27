//! Observational recovery of this application's protected removal records.
//! Inventory never starts a process, mutates a grant, or authorizes cleanup.
use super::absence::{AbsenceReservation, NotConfiguredRemoval};
use super::authorization::{require_finalized, RemovalJournal};
use super::cleanup::CleanupRemoval;
use super::*;
use crate::profile::absence_record::{AbsencePhase, AbsenceRecord};
use crate::removal::RemovalAdmission;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 1024;
const MAX_INTENTS: usize = 256;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryStatus {
    Verified,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryState {
    Reserved,
    Prepared,
    NotConfigured,
    Cleanup,
    RootRemoving,
    Completed,
    RecoveryRequired,
}
/// Retains independently verified records even when the complete inventory is
/// unavailable. The caller must seal admission rather than treat it as empty.
pub struct RecoveryInventory {
    pub status: InventoryStatus,
    pub records: Vec<RecoveredRemoval>,
}
/// Historical protected state and physical custody, not current action consent.
/// In particular this cannot reconstruct the original logout request/receipt.
pub enum RecoveredRemoval {
    Configured(Box<ManagedRecovery>),
    Absent(AbsentRecovery),
}
pub struct ManagedRecovery {
    reservation: RemovalReservation,
    lease: Option<ProfileLease>,
    state: RecoveryState,
    ancestors: Vec<File>,
}
pub struct AbsentRecovery {
    reservation: AbsenceReservation,
    ancestors: Vec<File>,
    state: RecoveryState,
}
impl RecoveredRemoval {
    pub fn held_intent(&mut self) -> HeldRemoval<'_> {
        match self {
            Self::Configured(v) => HeldRemoval {
                inner: HeldRecord::Managed(&mut v.reservation),
            },
            Self::Absent(v) => HeldRemoval {
                inner: HeldRecord::Absent(&mut v.reservation),
            },
        }
    }
    pub fn profile(&self) -> ProfileId {
        match self {
            Self::Configured(v) => v.reservation.record.registration.profile,
            Self::Absent(v) => v.reservation.record.profile,
        }
    }
    pub fn attempt(&self) -> RemovalAttemptId {
        match self {
            Self::Configured(v) => v.reservation.record.attempt,
            Self::Absent(v) => v.reservation.record.attempt,
        }
    }
    pub fn state(&self) -> RecoveryState {
        match self {
            Self::Configured(v) => v.state,
            Self::Absent(v) => v.state,
        }
    }
    pub fn incarnation(&self) -> Option<[u8; 16]> {
        match self {
            Self::Configured(v) => Some(v.reservation.record.registration.incarnation),
            Self::Absent(_) => None,
        }
    }
    pub fn retired_epoch(&self) -> Option<u64> {
        match self {
            Self::Configured(v) if v.reservation.record.retired_epoch != 0 => {
                Some(v.reservation.record.retired_epoch)
            }
            _ => None,
        }
    }
    pub fn reauthorize(
        mut self,
        admission: RemovalRetryAdmission,
    ) -> Result<PreparedRemoval, RetryFailure<Self>> {
        let result = (|| {
            let Self::Configured(v) = &mut self else {
                return Err(RemovalFault::Scope);
            };
            if v.state != RecoveryState::Prepared {
                return Err(RemovalFault::RecoveryRequired);
            }
            let r = &v.reservation.record;
            admission
                .current(
                    r.registration.profile,
                    r.attempt,
                    r.registration.incarnation,
                    r.retired_epoch,
                )
                .map_err(retry_fault)?;
            v.reservation
                .verify(v.lease.as_ref().ok_or(RemovalFault::RecoveryRequired)?)?;
            let r = &v.reservation.record;
            admission
                .current(
                    r.registration.profile,
                    r.attempt,
                    r.registration.incarnation,
                    r.retired_epoch,
                )
                .map_err(retry_fault)
        })();
        if let Err(fault) = result {
            return Err(RetryFailure {
                fault,
                custody: Box::new(self),
                admission,
            });
        }
        let Self::Configured(mut v) = self else {
            unreachable!("configured join checked")
        };
        let Some(lease) = v.lease.take() else {
            return Err(RetryFailure {
                fault: RemovalFault::RecoveryRequired,
                custody: Box::new(Self::Configured(v)),
                admission,
            });
        };
        Ok(PreparedRemoval {
            lease,
            reservation: v.reservation,
            local_deadline: admission.deadline(),
            evidence: PreparedEvidence::Retry(admission),
        })
    }
    /// Historical completion is only an observation. Canonical finalized
    /// cohorts and current namespace absence are independently joined here;
    /// a fresh explicit local retry is still required to retire its receipt.
    pub fn finalization_pending(
        &mut self,
        journal: &dyn RemovalJournal,
    ) -> Result<bool, RemovalFault> {
        if self.state() != RecoveryState::Completed {
            return Ok(false);
        }
        require_finalized(journal, self.profile())?;
        let (base, owner, parent) = match self {
            Self::Configured(v) => (
                v.reservation
                    .path
                    .parent()
                    .ok_or(RemovalFault::Scope)?
                    .to_owned(),
                owner_sid().map_err(|_| RemovalFault::Scope)?,
                v.reservation
                    .parent
                    .try_clone()
                    .map_err(|_| RemovalFault::Storage)?,
            ),
            Self::Absent(v) => (
                v.reservation
                    .path
                    .parent()
                    .ok_or(RemovalFault::Scope)?
                    .to_owned(),
                owner_sid().map_err(|_| RemovalFault::Scope)?,
                v.reservation
                    .parent
                    .try_clone()
                    .map_err(|_| RemovalFault::Storage)?,
            ),
        };
        self.held_intent()
            .verify(&base, &owner, &parent)
            .map_err(|_| RemovalFault::Scope)?;
        match std::fs::symlink_metadata(base.join(self.profile().leaf())) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            _ => Err(RemovalFault::RecoveryRequired),
        }
    }
    /// Explicit local retry only, never reconstruction of a provider request.
    pub fn resume_cleanup(
        self,
        admission: RemovalRetryAdmission,
        journal: Arc<dyn RemovalJournal>,
    ) -> Result<CleanupRemoval, RetryFailure<Self>> {
        let Self::Configured(v) = self else {
            return Err(RetryFailure {
                fault: RemovalFault::Scope,
                custody: Box::new(self),
                admission,
            });
        };
        if !matches!(
            v.state,
            RecoveryState::Cleanup | RecoveryState::RootRemoving | RecoveryState::Completed
        ) {
            return Err(RetryFailure {
                fault: RemovalFault::Scope,
                custody: Box::new(Self::Configured(v)),
                admission,
            });
        }
        let ManagedRecovery {
            reservation,
            lease,
            state,
            ancestors,
        } = *v;
        debug_assert!(lease.is_none());
        match CleanupRemoval::recovered(reservation, ancestors, admission, journal) {
            Ok(value) => Ok(value),
            Err(parts) => {
                let (fault, reservation, ancestors, admission) = *parts;
                Err(RetryFailure {
                    fault,
                    custody: Box::new(Self::Configured(Box::new(ManagedRecovery {
                        reservation,
                        lease,
                        state,
                        ancestors,
                    }))),
                    admission,
                })
            }
        }
    }
    /// Separate tagged no-owner retry; no fabricated incarnation or epoch.
    pub fn reauthorize_absence(
        self,
        profile: ProfileId,
        attempt: RemovalAttemptId,
        admission: RemovalAdmission,
        associations: Option<&mut dyn crate::connection::Associations>,
    ) -> Result<NotConfiguredRemoval, AbsenceRetryFailure> {
        if self.profile() != profile
            || self.attempt() != attempt
            || !matches!(self, Self::Absent(_))
        {
            return Err(AbsenceRetryFailure {
                fault: RemovalFault::Scope,
                custody: Box::new(self),
                admission,
            });
        }
        let Self::Absent(v) = self else {
            unreachable!("tag checked")
        };
        match NotConfiguredRemoval::recovered(v.reservation, admission, associations, v.ancestors) {
            Ok(value) => Ok(value),
            Err(parts) => {
                let (fault, reservation, admission, ancestors) = *parts;
                Err(AbsenceRetryFailure {
                    fault,
                    custody: Box::new(Self::Absent(AbsentRecovery {
                        reservation,
                        ancestors,
                        state: v.state,
                    })),
                    admission,
                })
            }
        }
    }
}
pub struct AbsenceRetryFailure {
    fault: RemovalFault,
    custody: Box<RecoveredRemoval>,
    admission: RemovalAdmission,
}
impl AbsenceRetryFailure {
    pub fn fault(&self) -> RemovalFault {
        self.fault
    }
    pub fn into_parts(self) -> (RecoveredRemoval, RemovalAdmission) {
        (*self.custody, self.admission)
    }
}
/// Fixed native location only. A missing never-created base is a verified empty
/// inventory; any other discovery/open error is explicitly unavailable.
pub fn recover_pending(deadline: Instant) -> RecoveryInventory {
    recheck_pending(&mut [], deadline)
}
/// Complete observation including existing exclusive held records. Returned
/// records are only newly discovered custody; existing owners are never replaced.
pub fn recheck_pending(held: &mut [HeldRemoval<'_>], deadline: Instant) -> RecoveryInventory {
    let failed = || RecoveryInventory {
        status: InventoryStatus::Unavailable,
        records: Vec::new(),
    };
    let deadline = deadline.min(Instant::now() + Duration::from_secs(10));
    if Instant::now() >= deadline {
        return failed();
    }
    let Ok(local) = known_local() else {
        return failed();
    };
    let mut ancestors = Vec::new();
    for path in local.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let Ok(pin) = crate::image::pin_directory(path) else {
            return failed();
        };
        ancestors.push(pin);
    }
    let Ok(owner) = owner_sid() else {
        return failed();
    };
    inventory_under(&local.join(LEAF), &owner, &ancestors, held, deadline)
}
pub(super) fn inventory_under(
    base: &Path,
    owner: &str,
    ancestors: &[File],
    held: &mut [HeldRemoval<'_>],
    deadline: Instant,
) -> RecoveryInventory {
    let mut inventory = RecoveryInventory {
        status: InventoryStatus::Verified,
        records: Vec::new(),
    };
    let mut seen = vec![false; held.len()];
    if held.len() > MAX_INTENTS
        || held.iter().enumerate().any(|(index, item)| {
            held[..index]
                .iter()
                .any(|prior| prior.profile() == item.profile())
        })
    {
        inventory.status = InventoryStatus::Unavailable;
        return inventory;
    }
    if Instant::now() >= deadline {
        inventory.status = InventoryStatus::Unavailable;
        return inventory;
    }
    match std::fs::symlink_metadata(base) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !held.is_empty() {
                inventory.status = InventoryStatus::Unavailable;
            }
            return inventory;
        }
        Err(_) => {
            inventory.status = InventoryStatus::Unavailable;
            return inventory;
        }
        Ok(_) => (),
    }
    let Ok(parent) = hold_directory(base, owner) else {
        inventory.status = InventoryStatus::Unavailable;
        return inventory;
    };
    let Ok(entries) = std::fs::read_dir(base) else {
        inventory.status = InventoryStatus::Unavailable;
        return inventory;
    };
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ENTRIES || Instant::now() >= deadline {
            inventory.status = InventoryStatus::Unavailable;
            break;
        }
        let Ok(entry) = entry else {
            inventory.status = InventoryStatus::Unavailable;
            continue;
        };
        let name = entry.file_name();
        let Some(name) = inventory_name(&name) else {
            inventory.status = InventoryStatus::Unavailable;
            continue;
        };
        // Case variants/trailing aliases can collide with canonical Windows
        // paths. They are not new authority and cannot be hidden as empty.
        if !name.to_ascii_lowercase().contains(".removing") {
            let ordinary = profile_leaf(&format!("{name}.removing")).is_some()
                && std::fs::symlink_metadata(entry.path()).is_ok_and(|metadata| {
                    metadata.is_dir() && metadata.file_attributes() & 0x400 == 0
                });
            if !ordinary {
                inventory.status = InventoryStatus::Unavailable;
            }
            continue;
        }
        let Some(profile) = profile_leaf(name) else {
            inventory.status = InventoryStatus::Unavailable;
            continue;
        };
        if let Some(index) = held.iter().position(|item| item.profile() == profile) {
            seen[index] = true;
            if held[index].verify(base, owner, &parent).is_err() {
                inventory.status = InventoryStatus::Unavailable;
            }
            continue;
        }
        if inventory.records.len() + held.len() >= MAX_INTENTS {
            inventory.status = InventoryStatus::Unavailable;
            break;
        }
        match recover_record(base, profile, owner, &parent, ancestors) {
            Ok(record) => inventory.records.push(record),
            Err(_) => inventory.status = InventoryStatus::Unavailable,
        }
    }
    if Instant::now() >= deadline || seen.iter().any(|found| !found) {
        inventory.status = InventoryStatus::Unavailable;
    }
    inventory
}
fn inventory_name(name: &OsStr) -> Option<&str> {
    name.to_str().filter(|value| value.is_ascii())
}
fn profile_leaf(name: &str) -> Option<ProfileId> {
    let hex = name.strip_suffix(".removing")?;
    if hex.len() != 32
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    ProfileId::from_native(bytes)
}
fn recover_record(
    base: &Path,
    profile: ProfileId,
    owner: &str,
    parent: &File,
    ancestors: &[File],
) -> Result<RecoveredRemoval, Error> {
    let path = intent_path(base, profile);
    let mut file = OpenOptions::new()
        .access_mode(0xc001_0000)
        .share_mode(0)
        .custom_flags(0x0020_0000)
        .open(&path)
        .map_err(|_| Error::RecoveryRequired)?;
    verify_file(&file, owner)?;
    let owner_hash = Sha256::digest(owner.as_bytes()).into();
    let bytes = read_variable(&mut file, JOURNAL_LENGTH)?;
    if let Some(record) = AbsenceRecord::decode(&bytes, profile, owner_hash) {
        if identity(parent)? != record.parent || identity(&file)? != record.journal {
            return Err(Error::Scope);
        }
        let clear = matches!(std::fs::symlink_metadata(base.join(profile.leaf())),Err(error) if error.kind()==std::io::ErrorKind::NotFound);
        let state = if !clear {
            RecoveryState::RecoveryRequired
        } else {
            match record.phase {
                AbsencePhase::Reserved => RecoveryState::Reserved,
                AbsencePhase::Prepared => RecoveryState::NotConfigured,
                AbsencePhase::Completed => RecoveryState::Completed,
            }
        };
        let reservation = AbsenceReservation {
            file,
            parent: parent.try_clone().map_err(|_| Error::Io)?,
            path,
            record,
        };
        let ancestors = ancestors
            .iter()
            .map(File::try_clone)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::Io)?;
        return Ok(RecoveredRemoval::Absent(AbsentRecovery {
            reservation,
            ancestors,
            state,
        }));
    }
    let record = Journal::decode(&bytes, profile, owner_hash).ok_or(Error::Scope)?;
    if identity(parent)? != record.parent || identity(&file)? != record.journal {
        return Err(Error::Scope);
    }
    let lease = if record.phase == Phase::Prepared {
        recover_prepared_lease(base, owner, &record.registration, parent, ancestors).ok()
    } else {
        None
    };
    let state = match (record.phase, lease.is_some()) {
        (Phase::Reserved, _) => RecoveryState::Reserved,
        (Phase::Prepared, true) => RecoveryState::Prepared,
        (Phase::Cleanup, _) => RecoveryState::Cleanup,
        (Phase::RootRemoving, _) => RecoveryState::RootRemoving,
        (Phase::Completed, _) => RecoveryState::Completed,
        _ => RecoveryState::RecoveryRequired,
    };
    let reservation = RemovalReservation {
        file,
        parent: parent.try_clone().map_err(|_| Error::Io)?,
        path,
        record,
    };
    Ok(RecoveredRemoval::Configured(Box::new(ManagedRecovery {
        reservation,
        lease,
        state,
        ancestors: ancestors
            .iter()
            .map(File::try_clone)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::Io)?,
    })))
}
/// Read-only exact reconstruction. Unlike normal open, this never creates a
/// missing node, migrates configuration, repairs DIRTY, or writes CLEAN.
pub(in crate::profile::native) fn recover_prepared_lease(
    base: &Path,
    owner: &str,
    record: &Record,
    parent: &File,
    ancestors: &[File],
) -> Result<ProfileLease, Error> {
    if record.version != crate::profile::VERSION || !record.policy_valid() {
        return Err(Error::Configuration);
    }
    let root = base.join(record.profile.leaf());
    let mut pins = Vec::with_capacity(ancestors.len() + 10);
    for pin in ancestors {
        pins.push(pin.try_clone().map_err(|_| Error::Io)?);
    }
    pins.push(parent.try_clone().map_err(|_| Error::Io)?);
    let root_hold = hold_metadata_directory(&root, owner)?;
    if identity(&root_hold)? != record.nodes[0] {
        return Err(Error::Scope);
    }
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(0x0020_0000)
        .open(root.join("lease"))
        .map_err(|_| Error::RecoveryRequired)?;
    verify_file(&lock, owner)?;
    if identity(&lock)? != record.nodes[7] || read_bounded(&mut lock, 8)? != CLEAN {
        return Err(Error::RecoveryRequired);
    }
    pins.push(root_hold);
    for (index, name) in NAMES.iter().enumerate() {
        let path = root.join(name);
        let pin = if index == 0 {
            hold_metadata_directory(&path, owner)?
        } else {
            hold_directory(&path, owner)?
        };
        if identity(&pin)? != record.nodes[index + 1] {
            return Err(Error::Scope);
        }
        pins.push(pin);
    }
    let home = root.join(NAMES[0]);
    require_absent(&home.join(CONFIG_STAGE))?;
    require_absent(&root.join(RECORD_STAGE))?;
    for (path, expected) in [
        (
            home.join("config.toml"),
            crate::profile::managed_config().as_bytes().to_vec(),
        ),
        (
            home.join("environments.toml"),
            crate::config::ENVIRONMENTS_TOML.as_bytes().to_vec(),
        ),
        (
            root.join("registration"),
            record.encode().ok_or(Error::Scope)?,
        ),
    ] {
        let mut file = open_read(&path)?;
        verify_file(&file, owner)?;
        if read_bounded(&mut file, expected.len())? != expected {
            return Err(Error::Configuration);
        }
        pins.push(file);
    }
    Ok(ProfileLease {
        root,
        record: record.clone(),
        _pins: pins,
        lock: Mutex::new(lock),
    })
}

#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, ProfileLease) {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("protected-owned-parent");
        create_dir(&base, &owner_sid().unwrap()).unwrap();
        let profile = ProfileId::from_native(random_id().unwrap()).unwrap();
        let lease = ProfileLease::fixture(&base, profile).unwrap();
        (root, base, lease)
    }
    fn scan(base: &Path) -> RecoveryInventory {
        inventory_under(
            base,
            &owner_sid().unwrap(),
            &[],
            &mut [],
            Instant::now() + Duration::from_secs(10),
        )
    }
    #[test]
    #[ignore = "owned synthetic historical Prepared journal only; no actual logout or erasure"]
    fn prepared_inventory_reconstructs_exact_clean_custody_without_changing_auth_or_identity() {
        let (root, base, lease) = fixture();
        let record = lease.record.clone();
        let sentinel = lease.root.join("codex-home/nonsecret-auth-sentinel");
        write_new(&sentinel, &owner_sid().unwrap(), b"owned-nonsecret-marker").unwrap();
        let mut reserved = lease
            .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 7)
            .unwrap();
        // Fixture bytes model a prior process's historical receipt. This does
        // not construct current PreparedRemoval or permission to erase.
        let historical = reserved.record.prepared(8).unwrap();
        let bytes = historical.encode().unwrap();
        write_lease(&mut reserved.file, &bytes).unwrap();
        drop(reserved);
        drop(lease);
        let mut inventory = scan(&base);
        assert_eq!(inventory.status, InventoryStatus::Verified);
        assert_eq!(inventory.records.len(), 1);
        {
            let mut held = [inventory.records[0].held_intent()];
            let rechecked = inventory_under(
                &base,
                &owner_sid().unwrap(),
                &[],
                &mut held,
                Instant::now() + Duration::from_secs(10),
            );
            assert_eq!(rechecked.status, InventoryStatus::Verified);
            assert!(
                rechecked.records.is_empty(),
                "existing exclusive custody is not reopened or replaced"
            );
        }
        let recovered = &inventory.records[0];
        assert_eq!(recovered.state(), RecoveryState::Prepared);
        assert_eq!(recovered.retired_epoch(), Some(8));
        let RecoveredRemoval::Configured(owned) = recovered else {
            panic!("configured tag")
        };
        assert_eq!(owned.lease.as_ref().unwrap().record, record);
        assert_eq!(std::fs::read(sentinel).unwrap(), b"owned-nonsecret-marker");
        drop(inventory);
        assert_eq!(
            std::fs::read(intent_path(&base, record.profile)).unwrap(),
            bytes
        );
        root.close().unwrap();
    }
    #[test]
    #[ignore = "owned synthetic incomplete protected inventory; no account or deletion calls"]
    fn incomplete_inventory_preserves_verified_reserved_records_and_never_becomes_empty() {
        let (root, base, lease) = fixture();
        lease.dirty().unwrap();
        let record = lease.record.clone();
        let reserved = lease
            .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 3)
            .unwrap();
        drop(reserved);
        drop(lease);
        let malformed = base.join("not-a-profile.ReMoViNg.extra");
        write_new(&malformed, &owner_sid().unwrap(), b"unrecognized-nonsecret").unwrap();
        let inventory = scan(&base);
        assert_eq!(inventory.status, InventoryStatus::Unavailable);
        assert_eq!(inventory.records.len(), 1);
        assert_eq!(inventory.records[0].profile(), record.profile);
        assert_eq!(inventory.records[0].state(), RecoveryState::Reserved);
        let RecoveredRemoval::Configured(owned) = &inventory.records[0] else {
            panic!("configured tag")
        };
        assert!(owned.lease.is_none());
        assert_eq!(
            std::fs::read(base.join(record.profile.leaf()).join("lease")).unwrap(),
            DIRTY
        );
        assert_eq!(std::fs::read(malformed).unwrap(), b"unrecognized-nonsecret");
        drop(inventory);
        root.close().unwrap();
    }
    #[test]
    #[ignore = "owned synthetic metadata mismatch refuses Prepared adoption without rewriting"]
    fn prepared_record_with_changed_configuration_stays_recovery_required() {
        let (root, base, lease) = fixture();
        let config = lease.root.join("codex-home/config.toml");
        let mut reserved = lease
            .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 2)
            .unwrap();
        let historical = reserved.record.prepared(3).unwrap();
        write_lease(&mut reserved.file, &historical.encode().unwrap()).unwrap();
        drop(reserved);
        drop(lease);
        std::fs::write(&config, b"unrecognized-nonsecret-config").unwrap();
        let inventory = scan(&base);
        assert_eq!(inventory.status, InventoryStatus::Verified);
        assert_eq!(inventory.records.len(), 1);
        assert_eq!(
            inventory.records[0].state(),
            RecoveryState::RecoveryRequired
        );
        let RecoveredRemoval::Configured(owned) = &inventory.records[0] else {
            panic!("configured tag")
        };
        assert!(owned.lease.is_none());
        assert_eq!(
            std::fs::read(config).unwrap(),
            b"unrecognized-nonsecret-config"
        );
        drop(inventory);
        root.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows private inventory grammar negatives; no account or deletion"]
    fn inventory_keeps_known_records_but_refuses_unknown_alias_file_and_reparse_entries() {
        for case in 0..5 {
            let (outer, base, lease) = fixture();
            assert_eq!(
                scan(&base).status,
                InventoryStatus::Verified,
                "live ordinary directory does not require reopening lease"
            );
            let reservation = lease
                .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 7)
                .unwrap();
            drop(reservation);
            drop(lease);
            let path = match case {
                0 => base.join("unknown-ascii-file"),
                1 => base.join("unknown-ascii-directory"),
                2 => base.join("ABCDEF01010101010101010101010101"),
                _ => base.join("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
            };
            if matches!(case, 0 | 3) {
                std::fs::write(&path, b"untouched").unwrap();
            } else {
                std::fs::create_dir(&path).unwrap();
            }
            if case == 4 {
                let target = outer.path().join("unrelated-target");
                std::fs::create_dir(&target).unwrap();
                std::fs::write(target.join("sentinel"), b"untouched").unwrap();
                super::super::fixtures::junction(&outer, &path, &target);
            }
            let observed = scan(&base);
            assert_eq!(observed.status, InventoryStatus::Unavailable);
            assert_eq!(observed.records.len(), 1);
            assert_eq!(observed.records[0].state(), RecoveryState::Reserved);
            assert!(std::fs::symlink_metadata(&path).is_ok());
            if case == 4 {
                assert_eq!(
                    std::fs::read(outer.path().join("unrelated-target/sentinel")).unwrap(),
                    b"untouched"
                );
                std::fs::remove_dir(&path).unwrap();
            }
            drop(observed);
            outer.close().unwrap();
        }
    }
    #[test]
    fn inventory_names_refuse_non_ascii_and_malformed_utf16_without_normalization() {
        use std::os::windows::ffi::OsStringExt;
        let malformed = std::ffi::OsString::from_wide(&[0xd800]);
        assert!(inventory_name(&malformed).is_none());
        for value in [
            "ſynthetic.removing",
            "01010101010101010101010101010101.removıng",
            "管理",
        ] {
            assert!(inventory_name(OsStr::new(value)).is_none());
        }
        assert_eq!(
            inventory_name(OsStr::new("01010101010101010101010101010101.removing")),
            Some("01010101010101010101010101010101.removing")
        );
    }
    #[test]
    fn closed_profile_leaf_parser_never_accepts_alias_or_zero_profile() {
        assert!(profile_leaf("01010101010101010101010101010101.removing").is_some());
        for name in [
            "00000000000000000000000000000000.removing",
            "ABCDEF01010101010101010101010101.removing",
            "../01010101010101010101010101010101.removing",
            "01010101010101010101010101010101.removing.extra",
        ] {
            assert!(profile_leaf(name).is_none());
        }
    }
}
