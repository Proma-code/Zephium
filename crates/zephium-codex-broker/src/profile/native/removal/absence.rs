//! Protected no-owner/no-home observation, distinct from a configured account
//! or logout receipt. The completion record survives until Store finalization.
use super::authorization::{require_authorized, require_finalized, RemovalJournal};
use super::*;
use crate::profile::absence_record::{AbsencePhase, AbsenceRecord, ABSENCE_LENGTH};
use crate::removal::RemovalAdmission;
use std::time::{Duration, Instant};

pub(super) struct AbsenceReservation {
    pub(super) file: File,
    pub(super) parent: File,
    pub(super) path: PathBuf,
    pub(super) record: AbsenceRecord,
}
impl AbsenceReservation {
    pub(super) fn verify(&mut self) -> Result<(), RemovalFault> {
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        if identity(&self.parent).map_err(|_| RemovalFault::Scope)? != self.record.parent
            || identity(&self.file).map_err(|_| RemovalFault::Scope)? != self.record.journal
            || <[u8; 32]>::from(Sha256::digest(owner.as_bytes())) != self.record.owner
        {
            return Err(RemovalFault::Scope);
        }
        verify_security(&self.parent, &owner, true).map_err(|_| RemovalFault::Scope)?;
        verify_file(&self.file, &owner).map_err(|_| RemovalFault::Scope)?;
        if AbsenceRecord::decode(
            &read_bounded(&mut self.file, ABSENCE_LENGTH).map_err(|_| RemovalFault::Storage)?,
            self.record.profile,
            self.record.owner,
        ) != Some(self.record.clone())
        {
            return Err(RemovalFault::RecoveryRequired);
        }
        Ok(())
    }
    pub(super) fn write_phase(&mut self, phase: AbsencePhase) -> Result<(), RemovalFault> {
        let mut record = self.record.clone();
        record.phase = phase;
        let bytes = record.encode().ok_or(RemovalFault::Scope)?;
        write_lease(&mut self.file, &bytes).map_err(|_| RemovalFault::Storage)?;
        if read_bounded(&mut self.file, ABSENCE_LENGTH).map_err(|_| RemovalFault::Storage)? != bytes
        {
            return Err(RemovalFault::RecoveryRequired);
        }
        self.record = record;
        Ok(())
    }
}
/// Same explicit app operation, canonical no-owner observation and protected
/// receipt. NoData never means that all historical vault artifacts were erased.
pub struct NotConfiguredRemoval {
    profile: ProfileId,
    attempt: RemovalAttemptId,
    base: PathBuf,
    base_id: NodeId,
    pins: Vec<File>,
    pub(super) reservation: Option<AbsenceReservation>,
    admission: RemovalAdmission,
    deadline: Instant,
    retirement_requested: bool,
}
type AbsenceRecoveryError = Box<(
    RemovalFault,
    AbsenceReservation,
    RemovalAdmission,
    Vec<File>,
)>;
impl NotConfiguredRemoval {
    /// Native admissions for this explicit Delete must already be retired.
    /// The fixed receipt parent may be created; no Codex home is ever created.
    pub fn from_native(
        profile: ProfileId,
        attempt: RemovalAttemptId,
        admission: RemovalAdmission,
        associations: &mut dyn crate::connection::Associations,
    ) -> Result<Self, RemovalFault> {
        let deadline = Instant::now() + Duration::from_secs(10);
        admission.current().map_err(|_| RemovalFault::Interrupted)?;
        require_unbound(associations)?;
        if Instant::now() >= deadline {
            return Err(RemovalFault::Interrupted);
        }
        let local = known_local().map_err(|_| RemovalFault::Storage)?;
        let mut pins = Vec::new();
        for path in local.ancestors().collect::<Vec<_>>().into_iter().rev() {
            pins.push(crate::image::pin_directory(path).map_err(|_| RemovalFault::Scope)?);
        }
        let base = local.join(LEAF);
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        admission.current().map_err(|_| RemovalFault::Interrupted)?;
        if Instant::now() >= deadline {
            return Err(RemovalFault::Interrupted);
        }
        match std::fs::symlink_metadata(&base) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_dir(&base, &owner).map_err(|_| RemovalFault::Storage)?
            }
            Err(_) => return Err(RemovalFault::Storage),
            Ok(_) => (),
        }
        Self::under(
            base,
            profile,
            attempt,
            admission,
            associations,
            pins,
            deadline,
        )
    }
    fn under(
        base: PathBuf,
        profile: ProfileId,
        attempt: RemovalAttemptId,
        admission: RemovalAdmission,
        associations: &mut dyn crate::connection::Associations,
        mut pins: Vec<File>,
        deadline: Instant,
    ) -> Result<Self, RemovalFault> {
        require_unbound(associations)?;
        if Instant::now() >= deadline {
            return Err(RemovalFault::Interrupted);
        }
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        let parent = hold_directory(&base, &owner).map_err(|_| RemovalFault::Scope)?;
        let parent_id = identity(&parent).map_err(|_| RemovalFault::Scope)?;
        absent(&base.join(profile.leaf()))?;
        let path = intent_path(&base, profile);
        absent(&path)?;
        admission.current().map_err(|_| RemovalFault::Interrupted)?;
        let sd = Descriptor::new(&owner, false).map_err(|_| RemovalFault::Scope)?;
        let name = wide(path.as_os_str()).map_err(|_| RemovalFault::Scope)?;
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
        .map_err(|_| RemovalFault::Storage)?;
        let file = unsafe { File::from_raw_handle(raw.0) };
        verify_file(&file, &owner).map_err(|_| RemovalFault::Scope)?;
        let record = AbsenceRecord {
            phase: AbsencePhase::Reserved,
            profile,
            attempt,
            owner: Sha256::digest(owner.as_bytes()).into(),
            parent: parent_id,
            journal: identity(&file).map_err(|_| RemovalFault::Scope)?,
        };
        let mut reservation = AbsenceReservation {
            file,
            parent,
            path,
            record,
        };
        reservation.write_phase(AbsencePhase::Reserved)?;
        pins.push(
            reservation
                .parent
                .try_clone()
                .map_err(|_| RemovalFault::Storage)?,
        );
        let mut value = Self {
            profile,
            attempt,
            base,
            base_id: parent_id,
            pins,
            reservation: Some(reservation),
            admission,
            deadline,
            retirement_requested: false,
        };
        value.current_namespace()?;
        require_unbound(associations)?;
        value.current_namespace()?;
        value
            .reservation
            .as_mut()
            .ok_or(RemovalFault::Scope)?
            .write_phase(AbsencePhase::Prepared)?;
        value.current_namespace()?;
        Ok(value)
    }
    pub(super) fn recovered(
        reservation: AbsenceReservation,
        admission: RemovalAdmission,
        associations: Option<&mut dyn crate::connection::Associations>,
        ancestors: Vec<File>,
    ) -> Result<Self, AbsenceRecoveryError> {
        let profile = reservation.record.profile;
        let attempt = reservation.record.attempt;
        let Some(base) = reservation.path.parent().map(Path::to_path_buf) else {
            return Err(Box::new((
                RemovalFault::Scope,
                reservation,
                admission,
                ancestors,
            )));
        };
        let base_id = reservation.record.parent;
        let parent_pin = match reservation.parent.try_clone() {
            Ok(pin) => pin,
            Err(_) => {
                return Err(Box::new((
                    RemovalFault::Storage,
                    reservation,
                    admission,
                    ancestors,
                )))
            }
        };
        let mut ancestors = ancestors;
        ancestors.push(parent_pin);
        let mut value = Self {
            profile,
            attempt,
            base,
            base_id,
            pins: ancestors,
            reservation: Some(reservation),
            admission,
            deadline: Instant::now() + Duration::from_secs(10),
            retirement_requested: false,
        };
        let result = (|| {
            value.current_namespace()?;
            if value
                .reservation
                .as_ref()
                .ok_or(RemovalFault::Scope)?
                .record
                .phase
                == AbsencePhase::Reserved
            {
                require_unbound(associations.ok_or(RemovalFault::Scope)?)?;
                value.current_namespace()?;
                value
                    .reservation
                    .as_mut()
                    .ok_or(RemovalFault::Scope)?
                    .write_phase(AbsencePhase::Prepared)?;
            }
            value.current_namespace()
        })();
        match result {
            Ok(()) => Ok(value),
            Err(fault) => Err(Box::new((
                fault,
                value.reservation.take().expect("retained reservation"),
                value.admission,
                value.pins,
            ))),
        }
    }
    pub fn profile(&self) -> ProfileId {
        self.profile
    }
    pub fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }
    pub fn held_intent(&mut self) -> Option<HeldRemoval<'_>> {
        self.reservation.as_mut().map(|reservation| HeldRemoval {
            inner: HeldRecord::Absent(reservation),
        })
    }
    pub fn completed(&self) -> bool {
        self.retirement_requested
            || self
                .reservation
                .as_ref()
                .is_some_and(|r| r.record.phase == AbsencePhase::Completed)
    }
    fn current_namespace(&mut self) -> Result<(), RemovalFault> {
        self.admission
            .current()
            .map_err(|_| RemovalFault::Interrupted)?;
        if Instant::now() >= self.deadline {
            return Err(RemovalFault::Interrupted);
        }
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        let parent = hold_directory(&self.base, &owner).map_err(|_| RemovalFault::Scope)?;
        if identity(&parent).map_err(|_| RemovalFault::Scope)? != self.base_id {
            return Err(RemovalFault::Scope);
        }
        if let Some(reservation) = self.reservation.as_mut() {
            reservation.verify()?;
        } else if !self.retirement_requested {
            return Err(RemovalFault::Scope);
        }
        absent(&self.base.join(self.profile.leaf()))?;
        self.admission
            .current()
            .map_err(|_| RemovalFault::Interrupted)?;
        if Instant::now() >= self.deadline {
            return Err(RemovalFault::Interrupted);
        }
        Ok(())
    }
    /// Pure retained-action admission for the serialized application actor.
    /// This is NOT a fresh namespace/Store observation and must never replace
    /// worker-side check_current/authorization/finalization verification.
    pub fn check_admission(&self) -> Result<(), RemovalFault> {
        self.admission
            .current()
            .map_err(|_| RemovalFault::Interrupted)?;
        if Instant::now() >= self.deadline {
            return Err(RemovalFault::Interrupted);
        }
        match self.reservation.as_ref() {
            Some(value)
                if value.record.profile != self.profile || value.record.attempt != self.attempt =>
            {
                Err(RemovalFault::Scope)
            }
            Some(value) => match value.record.phase {
                AbsencePhase::Reserved => Err(RemovalFault::RecoveryRequired),
                AbsencePhase::Prepared | AbsencePhase::Completed => Ok(()),
            },
            None if self.retirement_requested => Ok(()),
            None => Err(RemovalFault::Scope),
        }
    }
    pub fn check_current(&mut self) -> Result<(), RemovalFault> {
        self.current_namespace()?;
        if self
            .reservation
            .as_ref()
            .is_some_and(|r| r.record.phase == AbsencePhase::Reserved)
        {
            Err(RemovalFault::RecoveryRequired)
        } else {
            Ok(())
        }
    }
    pub fn verify_after_authorization(
        &mut self,
        journal: &dyn RemovalJournal,
    ) -> Result<(), RemovalFault> {
        self.check_current()?;
        require_authorized(journal, self.profile)?;
        self.current_namespace()?;
        let reservation = self.reservation.as_mut().ok_or(RemovalFault::Scope)?;
        if reservation.record.phase != AbsencePhase::Completed {
            reservation.write_phase(AbsencePhase::Completed)?;
        }
        self.current_namespace()
    }
    pub fn retry(
        &mut self,
        profile: ProfileId,
        attempt: RemovalAttemptId,
        admission: RemovalAdmission,
    ) -> Result<(), RemovalFault> {
        if profile != self.profile || attempt != self.attempt {
            return Err(RemovalFault::Scope);
        }
        admission.current().map_err(|_| RemovalFault::Interrupted)?;
        self.admission = admission;
        self.deadline = Instant::now() + Duration::from_secs(10);
        self.current_namespace()
    }
    pub fn acknowledge_finalization(
        &mut self,
        journal: &dyn RemovalJournal,
    ) -> Result<(), RemovalFault> {
        self.check_current()?;
        if !self.completed() {
            return Err(RemovalFault::Scope);
        }
        require_finalized(journal, self.profile)?;
        self.current_namespace()?;
        if let Some(reservation) = self.reservation.as_ref() {
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
            unsafe {
                SetFileInformationByHandle(
                    HANDLE(reservation.file.as_raw_handle()),
                    FileDispositionInfo,
                    (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            }
            .map_err(|_| RemovalFault::Storage)?;
            self.retirement_requested = true;
            drop(self.reservation.take());
        }
        absent(&intent_path(&self.base, self.profile))?;
        // Ancestor pins remain retained until the opaque finalization object drops.
        let _ = &self.pins;
        Ok(())
    }
}
fn absent(path: &Path) -> Result<(), RemovalFault> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(RemovalFault::RecoveryRequired),
    }
}
fn require_unbound(
    associations: &mut dyn crate::connection::Associations,
) -> Result<(), RemovalFault> {
    if associations.call(zephium_core::codex_store::Call::InspectOwner)
        == zephium_core::codex_store::Reply::Owner(None)
    {
        Ok(())
    } else {
        Err(RemovalFault::Scope)
    }
}

#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc,
    };
    struct Unbound;
    impl crate::connection::Associations for Unbound {
        fn call(
            &mut self,
            call: zephium_core::codex_store::Call,
        ) -> zephium_core::codex_store::Reply {
            assert_eq!(call, zephium_core::codex_store::Call::InspectOwner);
            zephium_core::codex_store::Reply::Owner(None)
        }
    }
    struct JournalView {
        profile: ProfileId,
        pending: bool,
        registered: bool,
    }
    impl RemovalJournal for JournalView {
        fn pending_profile_deletions(&self) -> zephium_core::ports::store::ProfileDeletionLoad {
            zephium_core::ports::store::ProfileDeletionLoad::Loaded(if self.pending {
                vec![zephium_core::ports::store::PendingProfileDeletion {
                    profile: zephium_core::ids::ProfileId::from(u128::from_be_bytes(
                        self.profile.bytes(),
                    )),
                    native_erasure_verified: false,
                }]
            } else {
                vec![]
            })
        }
        fn registered_profiles(&self) -> Option<Vec<zephium_core::ids::ProfileId>> {
            Some(if self.registered {
                vec![zephium_core::ids::ProfileId::from(u128::from_be_bytes(
                    self.profile.bytes(),
                ))]
            } else {
                vec![]
            })
        }
    }
    fn action() -> RemovalAdmission {
        RemovalAdmission::from_native(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(1)),
            1,
        )
        .unwrap()
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, NotConfiguredRemoval) {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("owned-receipts");
        create_dir(&base, &owner_sid().unwrap()).unwrap();
        let profile = ProfileId::from_native(random_id().unwrap()).unwrap();
        let attempt = RemovalAttemptId::fresh_native().unwrap();
        let value = NotConfiguredRemoval::under(
            base.clone(),
            profile,
            attempt,
            action(),
            &mut Unbound,
            vec![],
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        (root, base, value)
    }
    #[test]
    #[ignore = "owned Windows no-owner receipt only; no Codex home/auth/grant"]
    fn no_data_receipt_survives_authorization_boundary_and_never_becomes_configured_identity() {
        let (root, base, mut value) = fixture();
        let profile = value.profile();
        let attempt = value.attempt();
        assert!(!base.join(profile.leaf()).exists());
        assert_eq!(
            value.reservation.as_ref().unwrap().record.phase,
            AbsencePhase::Prepared
        );
        assert!(matches!(
            ProfileLease::fixture(&base, profile),
            Err(Error::RecoveryRequired)
        ));
        value.check_current().unwrap();
        drop(value);
        let mut inventory = super::super::recovery::inventory_under(
            &base,
            &owner_sid().unwrap(),
            &[],
            &mut [],
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(
            inventory.status,
            super::super::recovery::InventoryStatus::Verified
        );
        assert_eq!(inventory.records.len(), 1);
        let record = inventory.records.pop().unwrap();
        assert_eq!(record.incarnation(), None);
        assert_eq!(record.retired_epoch(), None);
        let mut resumed = record
            .reauthorize_absence(profile, attempt, action(), None)
            .ok()
            .unwrap();
        let mut journal = JournalView {
            profile,
            pending: false,
            registered: false,
        };
        assert_eq!(
            resumed.verify_after_authorization(&journal),
            Err(RemovalFault::Scope)
        );
        journal.pending = true;
        resumed.verify_after_authorization(&journal).unwrap();
        assert!(resumed.completed());
        assert_eq!(
            resumed.acknowledge_finalization(&journal),
            Err(RemovalFault::Scope)
        );
        journal.pending = false;
        journal.registered = true;
        assert_eq!(
            resumed.acknowledge_finalization(&journal),
            Err(RemovalFault::Scope)
        );
        journal.registered = false;
        resumed.acknowledge_finalization(&journal).unwrap();
        assert!(!intent_path(&base, profile).exists());
        assert!(!base.join(profile.leaf()).exists());
        drop(resumed);
        drop(inventory);
        root.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows unexpected managed root cannot be erased by absence proof"]
    fn appearing_root_blocks_absence_completion_without_erasing_it() {
        let (root, base, mut value) = fixture();
        let profile = value.profile();
        create_dir(&base.join(profile.leaf()), &owner_sid().unwrap()).unwrap();
        let sentinel = base.join(profile.leaf()).join("nonsecret-sentinel");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        // Pure actor admission remains only a live action/phase fact. The
        // worker still refuses the newly appeared namespace and never erases it.
        assert_eq!(value.check_admission(), Ok(()));
        let deadline = value.deadline;
        value.deadline = Instant::now();
        assert_eq!(value.check_admission(), Err(RemovalFault::Interrupted));
        value.deadline = deadline; // Fixture-only clock control, no product rearm API.
        assert_eq!(
            value.verify_after_authorization(&JournalView {
                profile,
                pending: true,
                registered: false
            }),
            Err(RemovalFault::RecoveryRequired)
        );
        assert_eq!(std::fs::read(sentinel).unwrap(), b"unchanged");
        drop(value);
        root.close().unwrap();
    }
}
