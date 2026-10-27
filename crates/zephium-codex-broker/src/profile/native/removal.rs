//! Protected preparation only. No ordinary close, parsed journal or Drop erases
//! an account home. Cleanup requires a separate authorized app-journal join.
pub(crate) mod absence;
pub(crate) mod authorization;
pub(crate) mod cleanup;
mod descendant_security;
#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod fixtures;
pub(crate) mod recovery;
use super::*;
use crate::connection::{removal::RemovalEvidence, SettledRemoval};
use crate::profile::removal::{Journal, Phase, RemovalAttemptId, JOURNAL_LENGTH};
use crate::removal::{RemovalRetryAdmission, RemovalRetryFault};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovalFault {
    Interrupted,
    Scope,
    Storage,
    RecoveryRequired,
}
fn retry_fault(fault: RemovalRetryFault) -> RemovalFault {
    match fault {
        RemovalRetryFault::Scope => RemovalFault::Scope,
        RemovalRetryFault::Interrupted | RemovalRetryFault::Expired => RemovalFault::Interrupted,
    }
}
impl RemovalAttemptId {
    pub fn fresh_native() -> Result<Self, &'static str> {
        Self::from_native_bytes(random_id().map_err(|_| "removal-nonce")?).ok_or("removal-nonce")
    }
}
/// The exact CREATE_NEW journal and its parent stay held. Dropping this object
/// closes handles but deliberately leaves the protected intent on disk.
pub(crate) struct RemovalReservation {
    file: File,
    parent: File,
    path: PathBuf,
    record: Journal,
}
/// Original process settlement and observed local logout are joined here, but
/// this is still not Store profile-deletion authorization or remote revocation.
/// Borrow of already-held protected intent. Rechecking inventory must not
/// reopen its exclusive file or replace its owning capability.
pub struct HeldRemoval<'a> {
    inner: HeldRecord<'a>,
}
enum HeldRecord<'a> {
    Managed(&'a mut RemovalReservation),
    Absent(&'a mut absence::AbsenceReservation),
}
impl HeldRemoval<'_> {
    fn profile(&self) -> ProfileId {
        match &self.inner {
            HeldRecord::Managed(r) => r.record.registration.profile,
            HeldRecord::Absent(r) => r.record.profile,
        }
    }
    fn verify(&mut self, base: &Path, owner: &str, parent: &File) -> Result<(), Error> {
        match &mut self.inner {
            HeldRecord::Managed(r) => {
                if r.path != intent_path(base, r.record.registration.profile)
                    || identity(parent)? != r.record.parent
                    || identity(&r.parent)? != r.record.parent
                    || identity(&r.file)? != r.record.journal
                {
                    return Err(Error::Scope);
                }
                verify_security(&r.parent, owner, true)?;
                verify_file(&r.file, owner)?;
                if Journal::decode(
                    &read_bounded(&mut r.file, JOURNAL_LENGTH)?,
                    r.record.registration.profile,
                    Sha256::digest(owner.as_bytes()).into(),
                ) != Some(r.record.clone())
                {
                    return Err(Error::Scope);
                }
            }
            HeldRecord::Absent(r) => {
                if r.path != intent_path(base, r.record.profile)
                    || identity(parent)? != r.record.parent
                {
                    return Err(Error::Scope);
                }
                r.verify().map_err(|_| Error::Scope)?;
            }
        }
        Ok(())
    }
}
pub struct PreparedRemoval {
    lease: ProfileLease,
    reservation: RemovalReservation,
    evidence: PreparedEvidence,
    local_deadline: Instant,
}
enum PreparedEvidence {
    Original(RemovalEvidence),
    Retry(RemovalRetryAdmission),
}
impl PreparedEvidence {
    fn current(&self, record: &Journal) -> Result<(), RemovalFault> {
        match self {
            Self::Original(evidence) => evidence
                .check_current()
                .map_err(|_| RemovalFault::Interrupted),
            Self::Retry(admission) => admission
                .current(
                    record.registration.profile,
                    record.attempt,
                    record.registration.incarnation,
                    record.retired_epoch,
                )
                .map_err(retry_fault),
        }
    }
}
/// A refused explicit local retry retains the exact previous custody and the
/// new action. It never revives original provider requests.
pub struct RetryFailure<T> {
    fault: RemovalFault,
    custody: Box<T>,
    admission: RemovalRetryAdmission,
}
impl<T> RetryFailure<T> {
    pub fn fault(&self) -> RemovalFault {
        self.fault
    }
    pub fn into_parts(self) -> (T, RemovalRetryAdmission) {
        (*self.custody, self.admission)
    }
}
pub struct PreparationFailure {
    fault: RemovalFault,
    custody: Box<SettledRemoval>,
}
impl PreparationFailure {
    pub fn profile(&self) -> ProfileId {
        self.custody.profile()
    }
    pub fn attempt(&self) -> RemovalAttemptId {
        self.custody.attempt()
    }
    pub fn incarnation(&self) -> [u8; 16] {
        self.custody.incarnation()
    }
    pub fn retired_epoch(&self) -> u64 {
        self.custody.retired_epoch()
    }
    pub fn fault(&self) -> RemovalFault {
        self.fault
    }
    pub fn into_custody(self) -> SettledRemoval {
        *self.custody
    }
    pub fn held_intent(&mut self) -> HeldRemoval<'_> {
        self.custody.held_intent()
    }
    /// Only an already-synced Prepared record can replace the expired local
    /// action after a post-write interruption. Reserved/uncertain stays blocked.
    pub fn retry(
        self,
        admission: RemovalRetryAdmission,
    ) -> Result<PreparedRemoval, RetryFailure<Self>> {
        let old_fault = self.fault;
        let (lease, mut reservation, evidence) = self.custody.into_parts();
        let result = (|| {
            if reservation.record.phase != Phase::Prepared {
                return Err(RemovalFault::RecoveryRequired);
            }
            let r = &reservation.record;
            admission
                .current(
                    r.registration.profile,
                    r.attempt,
                    r.registration.incarnation,
                    r.retired_epoch,
                )
                .map_err(retry_fault)?;
            reservation.verify(&lease)?;
            let r = &reservation.record;
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
                custody: Box::new(Self {
                    fault: old_fault,
                    custody: Box::new(SettledRemoval::from_parts(lease, reservation, evidence)),
                }),
                admission,
            });
        }
        Ok(PreparedRemoval {
            lease,
            reservation,
            local_deadline: admission.deadline(),
            evidence: PreparedEvidence::Retry(admission),
        })
    }
}
fn intent_path(base: &Path, profile: ProfileId) -> PathBuf {
    base.join(format!("{}.removing", profile.leaf()))
}
pub(super) fn require_no_intent(base: &Path, profile: ProfileId) -> Result<(), Error> {
    match std::fs::symlink_metadata(intent_path(base, profile)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // Observational open never adopts, repairs, or deletes any intent.
        _ => Err(Error::RecoveryRequired),
    }
}
impl ProfileLease {
    pub(crate) fn reserve_removal(
        &self,
        attempt: RemovalAttemptId,
        grant_epoch: u64,
    ) -> Result<RemovalReservation, Error> {
        if !(1..i64::MAX as u64).contains(&grant_epoch) {
            return Err(Error::Scope);
        }
        let owner = owner_sid()?;
        if <[u8; 32]>::from(Sha256::digest(owner.as_bytes())) != self.record.owner {
            return Err(Error::Scope);
        }
        let base = self.root.parent().ok_or(Error::Scope)?;
        let parent = hold_directory(base, &owner)?;
        let parent_id = identity(&parent)?;
        if !self
            ._pins
            .iter()
            .any(|pin| identity(pin).ok() == Some(parent_id))
        {
            return Err(Error::Scope);
        }
        let root_id = self.record.nodes[0];
        if self
            ._pins
            .iter()
            .filter(|pin| identity(pin).ok() == Some(root_id))
            .count()
            != 1
        {
            return Err(Error::Scope);
        }
        {
            let mut lease = self.lock.lock().map_err(|_| Error::Invalid)?;
            verify_file(&lease, &owner)?;
            if identity(&lease)? != self.record.nodes[7] {
                return Err(Error::Scope);
            }
            let marker = read_bounded(&mut lease, 8)?;
            // DIRTY is allowed only because this is the original live lease;
            // it does not qualify recovery or physical settlement.
            if marker != CLEAN && marker != DIRTY {
                return Err(Error::RecoveryRequired);
            }
        }
        let path = intent_path(base, self.record.profile);
        let sd = Descriptor::new(&owner, false)?;
        let name = wide(path.as_os_str())?;
        let raw = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                0xc001_0000,
                FILE_SHARE_MODE(0),
                Some(&sd.attrs()),
                CREATE_NEW,
                FILE_FLAGS_AND_ATTRIBUTES(0x0020_0000),
                None,
            )
        }
        .map_err(|_| Error::RecoveryRequired)?;
        let mut file = unsafe { File::from_raw_handle(raw.0) };
        verify_file(&file, &owner)?;
        let record = Journal::reserved(
            attempt,
            grant_epoch,
            self.record.clone(),
            parent_id,
            identity(&file)?,
        )
        .ok_or(Error::Scope)?;
        let bytes = record.encode().ok_or(Error::Scope)?;
        // A partial/uncertain write leaves the file in place, blocking normal
        // adoption. No failure path silently deletes a possibly durable intent.
        write_lease(&mut file, &bytes)?;
        if read_bounded(&mut file, JOURNAL_LENGTH)? != bytes {
            return Err(Error::RecoveryRequired);
        }
        Ok(RemovalReservation {
            file,
            parent,
            path,
            record,
        })
    }
}
impl RemovalReservation {
    pub(crate) fn held_intent(&mut self) -> HeldRemoval<'_> {
        HeldRemoval {
            inner: HeldRecord::Managed(self),
        }
    }
    fn verify(&mut self, lease: &ProfileLease) -> Result<(), RemovalFault> {
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        if self.record.registration != lease.record
            || identity(&self.parent).map_err(|_| RemovalFault::Scope)? != self.record.parent
            || identity(&self.file).map_err(|_| RemovalFault::Scope)? != self.record.journal
            || self.path
                != intent_path(
                    lease.root.parent().ok_or(RemovalFault::Scope)?,
                    lease.record.profile,
                )
        {
            return Err(RemovalFault::Scope);
        }
        verify_security(&self.parent, &owner, true).map_err(|_| RemovalFault::Scope)?;
        verify_file(&self.file, &owner).map_err(|_| RemovalFault::Scope)?;
        let bytes = read_bounded(&mut self.file, JOURNAL_LENGTH)
            .map_err(|_| RemovalFault::RecoveryRequired)?;
        if Journal::decode(&bytes, lease.record.profile, lease.record.owner)
            != Some(self.record.clone())
        {
            return Err(RemovalFault::RecoveryRequired);
        }
        let mut lock = lease.lock.lock().map_err(|_| RemovalFault::Scope)?;
        if identity(&lock).map_err(|_| RemovalFault::Scope)? != lease.record.nodes[7]
            || read_bounded(&mut lock, 8).map_err(|_| RemovalFault::RecoveryRequired)? != CLEAN
        {
            return Err(RemovalFault::RecoveryRequired);
        }
        Ok(())
    }
}
impl PreparedRemoval {
    pub fn from_settled(settled: SettledRemoval) -> Result<Self, PreparationFailure> {
        if settled.check_current().is_err() {
            return Err(PreparationFailure {
                fault: RemovalFault::Interrupted,
                custody: Box::new(settled),
            });
        }
        let (lease, mut reservation, evidence) = settled.into_parts();
        let result = (|| -> Result<(), RemovalFault> {
            reservation.verify(&lease)?;
            if reservation.record.attempt != evidence.attempt()
                || lease.record.incarnation != evidence.incarnation()
            {
                return Err(RemovalFault::Scope);
            }
            let prepared = match reservation.record.phase {
                Phase::Reserved => reservation
                    .record
                    .prepared(evidence.retired_epoch())
                    .ok_or(RemovalFault::Scope)?,
                Phase::Prepared if reservation.record.retired_epoch == evidence.retired_epoch() => {
                    reservation.record.clone()
                }
                _ => return Err(RemovalFault::Scope),
            };
            evidence
                .check_current()
                .map_err(|_| RemovalFault::Interrupted)?;
            let bytes = prepared.encode().ok_or(RemovalFault::Scope)?;
            write_lease(&mut reservation.file, &bytes).map_err(|_| RemovalFault::Storage)?;
            if read_bounded(&mut reservation.file, JOURNAL_LENGTH)
                .map_err(|_| RemovalFault::RecoveryRequired)?
                != bytes
            {
                return Err(RemovalFault::RecoveryRequired);
            }
            reservation.record = prepared;
            // Cancellation after this durable historical fact retains the whole
            // custody. It does not publish a cleanup capability or erase intent.
            evidence
                .check_current()
                .map_err(|_| RemovalFault::Interrupted)?;
            Ok(())
        })();
        if let Err(fault) = result {
            return Err(PreparationFailure {
                fault,
                custody: Box::new(SettledRemoval::from_parts(lease, reservation, evidence)),
            });
        }
        Ok(Self {
            lease,
            reservation,
            evidence: PreparedEvidence::Original(evidence),
            local_deadline: Instant::now() + Duration::from_secs(10),
        })
    }
    pub fn held_intent(&mut self) -> HeldRemoval<'_> {
        HeldRemoval {
            inner: HeldRecord::Managed(&mut self.reservation),
        }
    }
    pub fn profile(&self) -> ProfileId {
        self.lease.record.profile
    }
    pub fn attempt(&self) -> RemovalAttemptId {
        self.reservation.record.attempt
    }
    pub fn incarnation(&self) -> [u8; 16] {
        self.lease.record.incarnation
    }
    pub fn retired_epoch(&self) -> u64 {
        self.reservation.record.retired_epoch
    }
    pub fn check_current(&self) -> Result<(), RemovalFault> {
        if Instant::now() >= self.local_deadline {
            return Err(RemovalFault::Interrupted);
        }
        if self.reservation.record.phase != Phase::Prepared {
            return Err(RemovalFault::Scope);
        }
        self.evidence.current(&self.reservation.record)
    }
    pub fn retry(mut self, admission: RemovalRetryAdmission) -> Result<Self, RetryFailure<Self>> {
        let result = (|| {
            let record = &self.reservation.record;
            if record.phase != Phase::Prepared {
                return Err(RemovalFault::Scope);
            }
            admission
                .current(
                    record.registration.profile,
                    record.attempt,
                    record.registration.incarnation,
                    record.retired_epoch,
                )
                .map_err(retry_fault)?;
            self.reservation.verify(&self.lease)?;
            let record = &self.reservation.record;
            admission
                .current(
                    record.registration.profile,
                    record.attempt,
                    record.registration.incarnation,
                    record.retired_epoch,
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
        self.local_deadline = admission.deadline();
        self.evidence = PreparedEvidence::Retry(admission);
        Ok(self)
    }
}

#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod tests {
    use super::*;
    #[test]
    fn unique_physical_custody_caps_can_move_to_the_native_participant_without_cloning() {
        fn assert_send<T: Send>() {}
        assert_send::<PreparedRemoval>();
        assert_send::<PreparationFailure>();
        assert_send::<recovery::RecoveredRemoval>();
        assert_send::<cleanup::CleanupRemoval>();
        assert_send::<absence::NotConfiguredRemoval>();
    }
    fn fixture() -> (tempfile::TempDir, ProfileLease) {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("protected-owned-parent");
        create_dir(&base, &owner_sid().unwrap()).unwrap();
        let profile = ProfileId::from_native(random_id().unwrap()).unwrap();
        let lease = ProfileLease::fixture(&base, profile).unwrap();
        (root, lease)
    }
    #[test]
    #[ignore = "owned Windows configured removal admission never provisions or migrates a missing home"]
    fn configured_removal_existing_only_admission_never_creates_replaces_or_repairs_metadata() {
        let outer = tempfile::tempdir().unwrap();
        let profile = ProfileId::from_native(random_id().unwrap()).unwrap();
        let owner = owner_sid().unwrap();
        let base = outer.path().join("missing-owned-base");
        assert!(matches!(
            ProfileLease::existing_for_removal_under(&base, profile, [1; 16], &owner, &[]),
            Err(Error::RecoveryRequired)
        ));
        assert!(!base.exists());
        create_dir(&base, &owner).unwrap();
        assert!(matches!(
            ProfileLease::existing_for_removal_under(&base, profile, [1; 16], &owner, &[]),
            Err(Error::RecoveryRequired)
        ));
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
        let lease = ProfileLease::fixture(&base, profile).unwrap();
        let record = lease.record.clone();
        let root = lease.root.clone();
        let sentinel = root.join(NAMES[0]).join("nonsecret-auth-sentinel");
        std::fs::write(&sentinel, b"unchanged-owned-marker").unwrap();
        drop(lease);
        let original = std::fs::read(root.join("registration")).unwrap();
        assert!(matches!(
            ProfileLease::existing_for_removal_under(&base, profile, [99; 16], &owner, &[]),
            Err(Error::Scope)
        ));
        assert_eq!(std::fs::read(root.join("registration")).unwrap(), original);
        let reopened = ProfileLease::existing_for_removal_under(
            &base,
            profile,
            record.incarnation,
            &owner,
            &[],
        )
        .unwrap();
        assert_eq!(reopened.record, record);
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged-owned-marker");
        drop(reopened);
        let legacy = record.legacy().encode().unwrap();
        std::fs::write(root.join("registration"), &legacy).unwrap();
        std::fs::write(
            root.join(NAMES[0]).join("config.toml"),
            crate::config::CONFIG_TOML,
        )
        .unwrap();
        assert!(matches!(
            ProfileLease::existing_for_removal_under(
                &base,
                profile,
                record.incarnation,
                &owner,
                &[]
            ),
            Err(Error::RecoveryRequired)
        ));
        assert_eq!(std::fs::read(root.join("registration")).unwrap(), legacy);
        assert_eq!(
            std::fs::read(root.join(NAMES[0]).join("config.toml")).unwrap(),
            crate::config::CONFIG_TOML.as_bytes()
        );
        assert_eq!(std::fs::read(root.join("lease")).unwrap(), CLEAN);
        std::fs::write(root.join("registration"), &original).unwrap();
        std::fs::write(
            root.join(NAMES[0]).join("config.toml"),
            crate::profile::managed_config().as_bytes(),
        )
        .unwrap();
        std::fs::write(root.join("lease"), DIRTY).unwrap();
        assert!(matches!(
            ProfileLease::existing_for_removal_under(
                &base,
                profile,
                record.incarnation,
                &owner,
                &[]
            ),
            Err(Error::RecoveryRequired)
        ));
        assert_eq!(std::fs::read(root.join("lease")).unwrap(), DIRTY);
        std::fs::write(root.join("lease"), CLEAN).unwrap();
        std::fs::remove_file(root.join("lease")).unwrap();
        assert!(matches!(
            ProfileLease::existing_for_removal_under(
                &base,
                profile,
                record.incarnation,
                &owner,
                &[]
            ),
            Err(Error::RecoveryRequired)
        ));
        assert!(!root.join("lease").exists());
        assert_eq!(std::fs::read(root.join("registration")).unwrap(), original);
        // Deliberately remove ONLY this disposable synthetic root; observational
        // admission must not recreate the formerly configured incarnation.
        std::fs::remove_dir_all(&root).unwrap();
        assert!(matches!(
            ProfileLease::existing_for_removal_under(
                &base,
                profile,
                record.incarnation,
                &owner,
                &[]
            ),
            Err(Error::RecoveryRequired)
        ));
        assert!(!root.exists());
        outer.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows removal reservation only; no account/logout/deletion calls"]
    fn reserved_intent_is_bound_exclusive_and_blocks_observational_reopen_without_erasure() {
        let (root, lease) = fixture();
        let sentinel = lease.root.join("codex-home/nonsecret-auth-sentinel");
        write_new(&sentinel, &owner_sid().unwrap(), b"owned-nonsecret-marker").unwrap();
        let attempt = RemovalAttemptId::fresh_native().unwrap();
        let mut reservation = lease.reserve_removal(attempt, 11).unwrap();
        assert_eq!(reservation.record.phase, Phase::Reserved);
        assert_eq!(reservation.record.attempt, attempt);
        assert_eq!(reservation.record.registration, lease.record);
        assert_eq!(reservation.record.reserved_epoch, 11);
        assert_eq!(reservation.record.retired_epoch, 0);
        assert_eq!(
            identity(&reservation.file).unwrap(),
            reservation.record.journal
        );
        let bytes = read_bounded(&mut reservation.file, JOURNAL_LENGTH).unwrap();
        assert_eq!(bytes, reservation.record.encode().unwrap());
        assert!(lease
            .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 11)
            .is_err());
        let base = lease.root.parent().unwrap().to_path_buf();
        let profile = lease.record.profile;
        drop(reservation);
        drop(lease);
        assert!(matches!(
            ProfileLease::fixture(&base, profile),
            Err(Error::RecoveryRequired)
        ));
        assert_eq!(std::fs::read(sentinel).unwrap(), b"owned-nonsecret-marker");
        assert_eq!(std::fs::read(intent_path(&base, profile)).unwrap(), bytes);
        root.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows malformed removal-intent refusal; no account/provider calls"]
    fn unknown_or_torn_intent_is_never_adopted_overwritten_or_erased() {
        let (root, lease) = fixture();
        let base = lease.root.parent().unwrap().to_path_buf();
        let profile = lease.record.profile;
        let path = intent_path(&base, profile);
        write_new(&path, &owner_sid().unwrap(), b"unknown-nonsecret-intent").unwrap();
        assert!(lease
            .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 1)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unknown-nonsecret-intent");
        drop(lease);
        assert!(matches!(
            ProfileLease::fixture(&base, profile),
            Err(Error::RecoveryRequired)
        ));
        assert_eq!(std::fs::read(path).unwrap(), b"unknown-nonsecret-intent");
        root.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows live-DIRTY reservation is not settlement; no child/account calls"]
    fn live_dirty_reserved_record_does_not_turn_into_prepared_or_clean_on_drop() {
        let (root, lease) = fixture();
        lease.dirty().unwrap();
        let reservation = lease
            .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 3)
            .unwrap();
        assert_eq!(reservation.record.phase, Phase::Reserved);
        assert_eq!(
            read_bounded(&mut lease.lock.lock().unwrap(), 8).unwrap(),
            DIRTY
        );
        let base = lease.root.parent().unwrap().to_path_buf();
        let profile = lease.record.profile;
        let marker = lease.root.join("lease");
        drop(reservation);
        drop(lease);
        assert_eq!(std::fs::read(marker).unwrap(), DIRTY);
        assert!(matches!(
            ProfileLease::fixture(&base, profile),
            Err(Error::RecoveryRequired)
        ));
        root.close().unwrap();
    }
}
