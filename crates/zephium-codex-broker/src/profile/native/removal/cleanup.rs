//! Exact owned-tree cleanup after canonical authorization. All interruption
//! paths retain the protected journal and the remaining physical custody.
use super::authorization::{require_authorized, require_finalized, RemovalJournal};
use super::*;
use std::sync::Arc;
const MAX_NODES: usize = 8192;
const MAX_DEPTH: usize = 32;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupProgress {
    Completed,
    Interrupted,
    Partial(RemovalFault),
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Checkpoint {
    FirstNode,
    LeaseRemoved,
    RootRemoved,
    CompletionSynced,
    ReceiptRemoved,
}
struct Node {
    path: PathBuf,
    id: NodeId,
    file: Option<File>,
    requested: bool,
}
/// Local namespace cleanup only. Original logout is observed separately; this
/// never claims global OAuth revocation or forensic erasure outside this root.
pub struct CleanupRemoval {
    root: PathBuf,
    record: Record,
    intent: Journal,
    pins: Vec<File>,
    lease: Option<Mutex<File>>,
    poisoned_lease: Option<File>,
    reservation: Option<RemovalReservation>,
    evidence: PreparedEvidence,
    deadline: Instant,
    journal: Arc<dyn RemovalJournal>,
    nodes: Vec<Node>,
    cursor: Option<usize>,
    pending_guard: Option<File>,
    lease_node: Option<Node>,
    retirement_requested: bool,
    #[cfg(test)]
    stop: Option<Checkpoint>,
}
impl PreparedRemoval {
    /// Merely transfers retained custody. The first step independently checks
    /// canonical authorization and syncs Cleanup before any old pin is released.
    pub fn into_cleanup(self, journal: Arc<dyn RemovalJournal>) -> CleanupRemoval {
        let ProfileLease {
            root,
            record,
            _pins,
            lock,
        } = self.lease;
        let intent = self.reservation.record.clone();
        CleanupRemoval {
            root,
            record,
            intent,
            pins: _pins,
            lease: Some(lock),
            poisoned_lease: None,
            reservation: Some(self.reservation),
            evidence: self.evidence,
            deadline: self.local_deadline,
            journal,
            nodes: Vec::new(),
            cursor: None,
            pending_guard: None,
            lease_node: None,
            retirement_requested: false,
            #[cfg(test)]
            stop: None,
        }
    }
}
type CleanupRecoveryError = Box<(
    RemovalFault,
    RemovalReservation,
    Vec<File>,
    RemovalRetryAdmission,
)>;
impl CleanupRemoval {
    pub(super) fn recovered(
        reservation: RemovalReservation,
        mut pins: Vec<File>,
        admission: RemovalRetryAdmission,
        journal: Arc<dyn RemovalJournal>,
    ) -> Result<Self, CleanupRecoveryError> {
        let r = &reservation.record;
        if !matches!(
            r.phase,
            Phase::Cleanup | Phase::RootRemoving | Phase::Completed
        ) {
            return Err(Box::new((
                RemovalFault::Scope,
                reservation,
                pins,
                admission,
            )));
        }
        if let Err(fault) = admission.current(
            r.registration.profile,
            r.attempt,
            r.registration.incarnation,
            r.retired_epoch,
        ) {
            return Err(Box::new((retry_fault(fault), reservation, pins, admission)));
        }
        let Some(base) = reservation.path.parent() else {
            return Err(Box::new((
                RemovalFault::Scope,
                reservation,
                pins,
                admission,
            )));
        };
        let root = base.join(r.registration.profile.leaf());
        let record = r.registration.clone();
        let intent = r.clone();
        let parent = match reservation.parent.try_clone() {
            Ok(value) => value,
            Err(_) => {
                return Err(Box::new((
                    RemovalFault::Storage,
                    reservation,
                    pins,
                    admission,
                )))
            }
        };
        pins.push(parent);
        Ok(Self {
            root,
            record,
            intent,
            pins,
            lease: None,
            poisoned_lease: None,
            reservation: Some(reservation),
            deadline: admission.deadline(),
            evidence: PreparedEvidence::Retry(admission),
            journal,
            nodes: Vec::new(),
            cursor: None,
            pending_guard: None,
            lease_node: None,
            retirement_requested: false,
            #[cfg(test)]
            stop: None,
        })
    }
    pub fn profile(&self) -> ProfileId {
        self.record.profile
    }
    pub fn attempt(&self) -> RemovalAttemptId {
        self.intent.attempt
    }
    pub fn incarnation(&self) -> [u8; 16] {
        self.record.incarnation
    }
    pub fn retired_epoch(&self) -> u64 {
        self.intent.retired_epoch
    }
    pub fn held_intent(&mut self) -> Option<HeldRemoval<'_>> {
        self.reservation
            .as_mut()
            .map(RemovalReservation::held_intent)
    }
    #[cfg(test)]
    fn checkpoint(&mut self, at: Checkpoint) -> Result<(), RemovalFault> {
        if self.stop == Some(at) {
            self.stop = None;
            Err(RemovalFault::Interrupted)
        } else {
            Ok(())
        }
    }
    fn current(&self) -> Result<(), RemovalFault> {
        if Instant::now() >= self.deadline {
            return Err(RemovalFault::Interrupted);
        }
        self.evidence.current(&self.intent)
    }
    fn phase(&self) -> Phase {
        self.intent.phase
    }
    pub fn completed(&self) -> bool {
        self.phase() == Phase::Completed
    }
    pub fn check_current(&self) -> Result<(), RemovalFault> {
        self.current()
    }

    fn write_phase(&mut self, phase: Phase) -> Result<(), RemovalFault> {
        self.current()?;
        let reservation = self.reservation.as_mut().ok_or(RemovalFault::Scope)?;
        let mut record = reservation.record.clone();
        record.phase = phase;
        let bytes = record.encode().ok_or(RemovalFault::Scope)?;
        write_lease(&mut reservation.file, &bytes).map_err(|_| RemovalFault::Storage)?;
        if read_bounded(&mut reservation.file, JOURNAL_LENGTH).map_err(|_| RemovalFault::Storage)?
            != bytes
        {
            return Err(RemovalFault::RecoveryRequired);
        }
        reservation.record = record.clone();
        self.intent = record;
        self.current()
    }
    fn verify_receipt(&mut self) -> Result<(), RemovalFault> {
        self.current()?;
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        let reservation = self.reservation.as_mut().ok_or(RemovalFault::Scope)?;
        if reservation.record != self.intent {
            return Err(RemovalFault::Scope);
        }
        verify_file(&reservation.file, &owner).map_err(|_| RemovalFault::Scope)?;
        verify_security(&reservation.parent, &owner, true).map_err(|_| RemovalFault::Scope)?;
        let parent = hold_directory(self.root.parent().ok_or(RemovalFault::Scope)?, &owner)
            .map_err(|_| RemovalFault::Scope)?;
        if identity(&parent).map_err(|_| RemovalFault::Scope)? != reservation.record.parent
            || identity(&reservation.file).map_err(|_| RemovalFault::Scope)?
                != reservation.record.journal
        {
            return Err(RemovalFault::Scope);
        }
        if Journal::decode(
            &read_bounded(&mut reservation.file, JOURNAL_LENGTH)
                .map_err(|_| RemovalFault::Storage)?,
            self.record.profile,
            self.record.owner,
        ) != Some(reservation.record.clone())
        {
            return Err(RemovalFault::RecoveryRequired);
        }
        self.current()
    }
    pub fn step(&mut self) -> CleanupProgress {
        match self.step_inner() {
            Ok(()) => CleanupProgress::Completed,
            Err(RemovalFault::Interrupted) => CleanupProgress::Interrupted,
            Err(fault) => CleanupProgress::Partial(fault),
        }
    }
    fn step_inner(&mut self) -> Result<(), RemovalFault> {
        self.verify_receipt()?;
        require_authorized(self.journal.as_ref(), self.record.profile)?;
        self.current()?;
        if self.phase() == Phase::Prepared {
            self.require_clean_lease()?;
            self.write_phase(Phase::Cleanup)?;
        }
        match self.phase() {
            Phase::Cleanup => {
                self.admit_cleanup_lease()?;
                self.require_clean_lease()?;
                if self.cursor.is_none() {
                    self.nodes.clear();
                    self.pending_guard = None;
                    self.collect(self.root.clone(), 0)?;
                    self.cursor = Some(self.nodes.len());
                }
                while self.cursor.ok_or(RemovalFault::Scope)? > 1 {
                    self.current()?;
                    let index = self.cursor.ok_or(RemovalFault::Scope)? - 1;
                    self.remove_node(index)?;
                    self.cursor = Some(index);
                    #[cfg(test)]
                    self.checkpoint(Checkpoint::FirstNode)?;
                }
                self.remove_lease()?;
                #[cfg(test)]
                self.checkpoint(Checkpoint::LeaseRemoved)?;
                self.current()?;
                let root = self.nodes.first().ok_or(RemovalFault::Scope)?;
                let file = root.file.as_ref().ok_or(RemovalFault::Scope)?;
                if identity(file).map_err(|_| RemovalFault::Scope)? != self.record.nodes[0] {
                    return Err(RemovalFault::Scope);
                }
                if std::fs::read_dir(&self.root)
                    .map_err(|_| RemovalFault::Storage)?
                    .next()
                    .is_some()
                {
                    return Err(RemovalFault::RecoveryRequired);
                }
                // Write-ahead authorization for THIS empty, exact held root.
                // Recovery may later establish namespace absence, not erase a replacement.
                self.write_phase(Phase::RootRemoving)?;
            }
            Phase::RootRemoving | Phase::Completed => (),
            _ => return Err(RemovalFault::Scope),
        }
        if self.phase() == Phase::RootRemoving {
            if self.nodes.is_empty() {
                self.collect_empty_root()?;
            }
            if !self.nodes.is_empty() {
                self.remove_node(0)?;
            }
            absent(&self.root)?;
            #[cfg(test)]
            self.checkpoint(Checkpoint::RootRemoved)?;
            self.write_phase(Phase::Completed)?;
            #[cfg(test)]
            self.checkpoint(Checkpoint::CompletionSynced)?;
        }
        if self.phase() != Phase::Completed {
            return Err(RemovalFault::Scope);
        }
        absent(&self.root)?;
        self.current()
    }
    fn admit_cleanup_lease(&mut self) -> Result<(), RemovalFault> {
        if self.lease.is_some() || self.lease_node.is_some() {
            return Ok(());
        }
        self.current()?;
        let path = self.root.join("lease");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(RemovalFault::Storage),
            Ok(_) => (),
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .custom_flags(0x0020_0000)
            .open(&path)
            .map_err(|_| RemovalFault::RecoveryRequired)?;
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        verify_file(&file, &owner).map_err(|_| RemovalFault::Scope)?;
        if identity(&file).map_err(|_| RemovalFault::Scope)? != self.record.nodes[7]
            || read_bounded(&mut file, 8).map_err(|_| RemovalFault::Storage)? != CLEAN
        {
            return Err(RemovalFault::RecoveryRequired);
        }
        self.lease = Some(Mutex::new(file));
        self.current()
    }
    fn require_clean_lease(&self) -> Result<(), RemovalFault> {
        if self.poisoned_lease.is_some() {
            return Err(RemovalFault::RecoveryRequired);
        }
        if let Some(lock) = &self.lease {
            let mut file = lock.lock().map_err(|_| RemovalFault::RecoveryRequired)?;
            let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
            verify_file(&file, &owner).map_err(|_| RemovalFault::Scope)?;
            if identity(&file).map_err(|_| RemovalFault::Scope)? != self.record.nodes[7]
                || read_bounded(&mut file, 8).map_err(|_| RemovalFault::Storage)? != CLEAN
            {
                return Err(RemovalFault::RecoveryRequired);
            }
        } else if let Some(node) = &self.lease_node {
            if let Some(file) = &node.file {
                let mut held = file.try_clone().map_err(|_| RemovalFault::Storage)?;
                if identity(&held).map_err(|_| RemovalFault::Scope)? != self.record.nodes[7]
                    || read_bounded(&mut held, 8).map_err(|_| RemovalFault::Storage)? != CLEAN
                {
                    return Err(RemovalFault::RecoveryRequired);
                }
            }
        } else if self.phase() == Phase::Prepared {
            return Err(RemovalFault::RecoveryRequired);
        }
        Ok(())
    }
    fn collect(&mut self, path: PathBuf, depth: usize) -> Result<(), RemovalFault> {
        self.current()?;
        if depth > MAX_DEPTH || self.nodes.len() >= MAX_NODES {
            return Err(RemovalFault::RecoveryRequired);
        }
        if path == self.root.join("lease") {
            return Ok(());
        }
        let guard = OpenOptions::new()
            .read(true)
            .share_mode(5)
            .custom_flags(0x0220_0000)
            .open(&path)
            .map_err(|_| RemovalFault::Storage)?;
        let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
        let directory =
            super::descendant_security::verify(&guard, &owner, self.record.nodes[0].volume)?;
        let id = identity(&guard).map_err(|_| RemovalFault::Scope)?;
        if self.nodes.iter().any(|node| node.id == id) {
            return Err(RemovalFault::Scope);
        }
        if depth == 0 {
            if id != self.record.nodes[0] || !directory {
                return Err(RemovalFault::Scope);
            }
            verify_security(&guard, &owner, true).map_err(|_| RemovalFault::Scope)?;
        } else if depth == 1 {
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or(RemovalFault::Scope)?;
            if let Some(index) = NAMES.iter().position(|known| *known == name) {
                if id != self.record.nodes[index + 1] || !directory {
                    return Err(RemovalFault::Scope);
                }
            } else if name != "registration" || directory {
                return Err(RemovalFault::Scope);
            }
        }
        if depth == 1 && directory {
            verify_security(&guard, &owner, true).map_err(|_| RemovalFault::Scope)?;
        }
        let expected = if path == self.root.join("registration") {
            Some(self.record.encode().ok_or(RemovalFault::Scope)?)
        } else if path == self.root.join(NAMES[0]).join("config.toml") {
            Some(crate::profile::managed_config().as_bytes().to_vec())
        } else if path == self.root.join(NAMES[0]).join("environments.toml") {
            Some(crate::config::ENVIRONMENTS_TOML.as_bytes().to_vec())
        } else {
            None
        };
        if let Some(expected) = expected {
            verify_file(&guard, &owner).map_err(|_| RemovalFault::Scope)?;
            let mut held = guard.try_clone().map_err(|_| RemovalFault::Storage)?;
            if read_bounded(&mut held, expected.len()).map_err(|_| RemovalFault::Scope)? != expected
            {
                return Err(RemovalFault::RecoveryRequired);
            }
        }
        self.pending_guard = Some(guard);
        // Only same-ID guards are released under the already-synced Cleanup
        // journal. A replacement is never adopted by a later pathname open.
        let mut index = 0;
        while index < self.pins.len() {
            if identity(&self.pins[index]).map_err(|_| RemovalFault::Scope)? == id {
                drop(self.pins.remove(index));
            } else {
                index += 1;
            }
        }
        self.current()?;
        let file = OpenOptions::new()
            .access_mode(0x8001_0000)
            .share_mode(1)
            .custom_flags(0x0220_0000)
            .open(&path)
            .map_err(|_| RemovalFault::Storage)?;
        if identity(&file).map_err(|_| RemovalFault::Scope)? != id
            || super::descendant_security::verify(&file, &owner, id.volume)? != directory
        {
            return Err(RemovalFault::Scope);
        }
        self.nodes.push(Node {
            path: path.clone(),
            id,
            file: Some(file),
            requested: false,
        });
        self.pending_guard = None;
        if directory {
            for entry in std::fs::read_dir(&path).map_err(|_| RemovalFault::Storage)? {
                self.current()?;
                let entry = entry.map_err(|_| RemovalFault::Storage)?;
                self.collect(entry.path(), depth + 1)?;
            }
        }
        Ok(())
    }
    fn remove_node(&mut self, index: usize) -> Result<(), RemovalFault> {
        self.current()?;
        let node = self.nodes.get_mut(index).ok_or(RemovalFault::Scope)?;
        if !node.requested {
            let file = node.file.as_ref().ok_or(RemovalFault::Scope)?;
            if identity(file).map_err(|_| RemovalFault::Scope)? != node.id {
                return Err(RemovalFault::Scope);
            }
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            unsafe {
                SetFileInformationByHandle(
                    HANDLE(file.as_raw_handle()),
                    FileDispositionInfo,
                    (&info as *const FILE_DISPOSITION_INFO).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            }
            .map_err(|_| RemovalFault::Storage)?;
            node.requested = true;
            drop(node.file.take());
        }
        absent(&node.path)?;
        self.current()
    }
    fn remove_lease(&mut self) -> Result<(), RemovalFault> {
        self.current()?;
        self.require_clean_lease()?;
        if let Some(lock) = self.lease.take() {
            match lock.into_inner() {
                Ok(file) => drop(file),
                Err(poison) => {
                    self.poisoned_lease = Some(poison.into_inner());
                    return Err(RemovalFault::RecoveryRequired);
                }
            }
        }
        let path = self.root.join("lease");
        if self.lease_node.is_none() {
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(_) => return Err(RemovalFault::Storage),
                Ok(_) => (),
            }
            let mut file = OpenOptions::new()
                .access_mode(0x8001_0000)
                .share_mode(0)
                .custom_flags(0x0020_0000)
                .open(&path)
                .map_err(|_| RemovalFault::Storage)?;
            let owner = owner_sid().map_err(|_| RemovalFault::Scope)?;
            verify_file(&file, &owner).map_err(|_| RemovalFault::Scope)?;
            if identity(&file).map_err(|_| RemovalFault::Scope)? != self.record.nodes[7]
                || read_bounded(&mut file, 8).map_err(|_| RemovalFault::Storage)? != CLEAN
            {
                return Err(RemovalFault::Scope);
            }
            self.lease_node = Some(Node {
                path,
                id: self.record.nodes[7],
                file: Some(file),
                requested: false,
            });
        }
        self.current()?;
        let node = self.lease_node.as_mut().ok_or(RemovalFault::Scope)?;
        if !node.requested {
            let file = node.file.as_ref().ok_or(RemovalFault::Scope)?;
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            unsafe {
                SetFileInformationByHandle(
                    HANDLE(file.as_raw_handle()),
                    FileDispositionInfo,
                    (&info as *const FILE_DISPOSITION_INFO).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            }
            .map_err(|_| RemovalFault::Storage)?;
            node.requested = true;
            drop(node.file.take());
        }
        absent(&node.path)?;
        self.current()
    }
    fn collect_empty_root(&mut self) -> Result<(), RemovalFault> {
        self.current()?;
        match std::fs::symlink_metadata(&self.root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(RemovalFault::Storage),
            Ok(_) => (),
        }
        self.collect(self.root.clone(), 0)?;
        if self.nodes.len() != 1 {
            return Err(RemovalFault::RecoveryRequired);
        }
        Ok(())
    }
    pub fn retry(&mut self, admission: RemovalRetryAdmission) -> Result<(), RemovalFault> {
        let r = &self.intent;
        admission
            .current(
                r.registration.profile,
                r.attempt,
                r.registration.incarnation,
                r.retired_epoch,
            )
            .map_err(retry_fault)?;
        self.deadline = admission.deadline();
        self.evidence = PreparedEvidence::Retry(admission);
        if self.phase() == Phase::Cleanup {
            self.nodes.clear();
            self.cursor = None;
            self.pending_guard = None;
        }
        if self.retirement_requested {
            require_finalized(self.journal.as_ref(), self.record.profile)?;
            absent(&self.root)?;
            return absent(&intent_path(
                self.root.parent().ok_or(RemovalFault::Scope)?,
                self.record.profile,
            ));
        }
        self.verify_receipt()
    }
    pub fn acknowledge_finalization(&mut self) -> Result<(), RemovalFault> {
        if self.retirement_requested {
            self.current()?;
            require_finalized(self.journal.as_ref(), self.record.profile)?;
            absent(&self.root)?;
            return absent(&intent_path(
                self.root.parent().ok_or(RemovalFault::Scope)?,
                self.record.profile,
            ));
        }
        self.verify_receipt()?;
        if self.phase() != Phase::Completed {
            return Err(RemovalFault::Scope);
        }
        require_finalized(self.journal.as_ref(), self.record.profile)?;
        absent(&self.root)?;
        self.current()?;
        let r = self.reservation.as_ref().ok_or(RemovalFault::Scope)?;
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        unsafe {
            SetFileInformationByHandle(
                HANDLE(r.file.as_raw_handle()),
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        }
        .map_err(|_| RemovalFault::Storage)?;
        self.retirement_requested = true;
        drop(self.reservation.take());
        #[cfg(test)]
        self.checkpoint(Checkpoint::ReceiptRemoved)?;
        absent(&intent_path(
            self.root.parent().ok_or(RemovalFault::Scope)?,
            self.record.profile,
        ))
    }
}
fn absent(path: &Path) -> Result<(), RemovalFault> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(RemovalFault::RecoveryRequired),
    }
}

#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod tests {
    use super::*;
    use crate::removal::RemovalAdmission;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    struct JournalView {
        profile: ProfileId,
        pending: AtomicBool,
        registered: AtomicBool,
    }
    impl RemovalJournal for JournalView {
        fn pending_profile_deletions(&self) -> zephium_core::ports::store::ProfileDeletionLoad {
            use zephium_core::ports::store::{PendingProfileDeletion, ProfileDeletionLoad};
            ProfileDeletionLoad::Loaded(if self.pending.load(Ordering::Acquire) {
                vec![PendingProfileDeletion {
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
            Some(if self.registered.load(Ordering::Acquire) {
                vec![zephium_core::ids::ProfileId::from(u128::from_be_bytes(
                    self.profile.bytes(),
                ))]
            } else {
                vec![]
            })
        }
    }
    fn retry(record: &Journal, cancelled: Arc<AtomicBool>) -> RemovalRetryAdmission {
        RemovalRetryAdmission::from_native_retry(
            record.registration.profile,
            record.attempt,
            record.registration.incarnation,
            record.retired_epoch,
            RemovalAdmission::from_native(cancelled, Arc::new(AtomicU64::new(1)), 1).unwrap(),
        )
        .unwrap()
    }
    fn fixture() -> (
        tempfile::TempDir,
        PathBuf,
        CleanupRemoval,
        Arc<JournalView>,
        Arc<AtomicBool>,
    ) {
        let outer = tempfile::tempdir().unwrap();
        let profile = ProfileId::from_native(random_id().unwrap()).unwrap();
        let lease = ProfileLease::fixture_protected(outer.path(), profile).unwrap();
        let base = lease.root.parent().unwrap().to_path_buf();
        let nested = lease.root.join(NAMES[0]).join("rollouts/synthetic-nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("nonsecret-fixture"), b"synthetic-no-credential").unwrap();
        let mut reservation = lease
            .reserve_removal(RemovalAttemptId::fresh_native().unwrap(), 7)
            .unwrap();
        // Synthetic historical bytes, not a fabricated live original-logout receipt.
        let historical = reservation.record.prepared(8).unwrap();
        write_lease(&mut reservation.file, &historical.encode().unwrap()).unwrap();
        reservation.record = historical;
        let cancel = Arc::new(AtomicBool::new(false));
        let admission = retry(&reservation.record, cancel.clone());
        let prepared = PreparedRemoval {
            lease,
            reservation,
            local_deadline: admission.deadline(),
            evidence: PreparedEvidence::Retry(admission),
        };
        let journal = Arc::new(JournalView {
            profile,
            pending: AtomicBool::new(true),
            registered: AtomicBool::new(false),
        });
        let cleanup = prepared.into_cleanup(journal.clone());
        (outer, base, cleanup, journal, cancel)
    }
    fn recover(base: &Path, journal: Arc<JournalView>) -> CleanupRemoval {
        let mut inventory = super::super::recovery::inventory_under(
            base,
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
        let admission = RemovalRetryAdmission::from_native_retry(
            record.profile(),
            record.attempt(),
            record.incarnation().unwrap(),
            record.retired_epoch().unwrap(),
            RemovalAdmission::from_native(
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU64::new(1)),
                1,
            )
            .unwrap(),
        )
        .unwrap();
        record.resume_cleanup(admission, journal).ok().unwrap()
    }
    #[test]
    #[ignore = "owned synthetic cleanup and retained completion only; no account or real data"]
    fn exact_owned_tree_cleanup_requires_authorization_and_keeps_completion_until_final_ack() {
        let (outer, base, mut cleanup, journal, cancel) = fixture();
        let root = cleanup.root.clone();
        let intent = intent_path(&base, cleanup.profile());
        journal.pending.store(false, Ordering::Release);
        assert_eq!(
            cleanup.step(),
            CleanupProgress::Partial(RemovalFault::Scope)
        );
        assert!(root
            .join(NAMES[0])
            .join("rollouts/synthetic-nested/nonsecret-fixture")
            .is_file());
        journal.pending.store(true, Ordering::Release);
        cancel.store(true, Ordering::Release);
        assert_eq!(cleanup.step(), CleanupProgress::Interrupted);
        assert_eq!(cleanup.phase(), Phase::Prepared);
        cleanup
            .retry(retry(&cleanup.intent, Arc::new(AtomicBool::new(false))))
            .unwrap();
        assert_eq!(cleanup.step(), CleanupProgress::Completed);
        assert!(!root.exists());
        assert!(intent.exists());
        assert_eq!(cleanup.acknowledge_finalization(), Err(RemovalFault::Scope));
        drop(cleanup);
        let mut cleanup = recover(&base, journal.clone());
        assert!(cleanup.completed());
        journal.pending.store(false, Ordering::Release);
        journal.registered.store(true, Ordering::Release);
        assert_eq!(cleanup.acknowledge_finalization(), Err(RemovalFault::Scope));
        journal.registered.store(false, Ordering::Release);
        cleanup.acknowledge_finalization().unwrap();
        assert!(!intent.exists());
        assert!(!root.exists());
        drop(cleanup);
        outer.close().unwrap();
    }
    #[test]
    #[ignore = "owned synthetic write-ahead root removal recovery; no real data"]
    fn root_removing_absence_recovers_but_cleanup_absence_and_replacement_refuse() {
        for phase in [Phase::Cleanup, Phase::RootRemoving, Phase::Completed] {
            let (outer, base, mut cleanup, journal, _) = fixture();
            assert_eq!(cleanup.step(), CleanupProgress::Completed);
            // Model interruption after physical root removal, before the next
            // phase acknowledgement; only RootRemoving was a write-ahead grant.
            cleanup.write_phase(phase).unwrap();
            drop(cleanup);
            let mut recovered = recover(&base, journal);
            let result = recovered.step();
            if phase == Phase::Cleanup {
                assert!(matches!(result, CleanupProgress::Partial(_)));
            } else {
                assert_eq!(result, CleanupProgress::Completed);
            }
            drop(recovered);
            outer.close().unwrap();
        }
        let (outer, base, mut cleanup, journal, _) = fixture();
        let root = cleanup.root.clone();
        assert_eq!(cleanup.step(), CleanupProgress::Completed);
        cleanup.write_phase(Phase::RootRemoving).unwrap();
        drop(cleanup);
        create_dir(&root, &owner_sid().unwrap()).unwrap();
        std::fs::write(root.join("unrelated-sentinel"), b"untouched").unwrap();
        let mut recovered = recover(&base, journal);
        assert!(matches!(recovered.step(), CleanupProgress::Partial(_)));
        assert_eq!(
            std::fs::read(root.join("unrelated-sentinel")).unwrap(),
            b"untouched"
        );
        drop(recovered);
        outer.close().unwrap();
    }
    #[test]
    #[ignore = "owned synthetic cleanup preflight negative controls; no real data"]
    fn preflight_unknown_hardlink_and_changed_fixed_metadata_never_start_deletion() {
        for case in 0..4 {
            let (outer, _, mut cleanup, _, _) = fixture();
            let root = cleanup.root.clone();
            match case {
                0 => {
                    std::fs::write(root.join("unknown-root-child"), b"untouched").unwrap();
                }
                1 => {
                    let source = root
                        .join(NAMES[0])
                        .join("rollouts/synthetic-nested/nonsecret-fixture");
                    std::fs::hard_link(source, root.join(NAMES[0]).join("extra-link")).unwrap();
                }
                2 => {
                    // Release only the held config pin in this negative fixture.
                    let config = root.join(NAMES[0]).join("config.toml");
                    let id = identity(
                        &OpenOptions::new()
                            .read(true)
                            .share_mode(5)
                            .open(&config)
                            .unwrap(),
                    )
                    .unwrap();
                    cleanup.pins.retain(|pin| identity(pin).unwrap() != id);
                    std::fs::write(config, b"unrecognized-fixed-config").unwrap();
                }
                3 => {
                    let target = outer.path().join("outside-owned-home");
                    std::fs::create_dir(&target).unwrap();
                    std::fs::write(target.join("sentinel"), b"untouched").unwrap();
                    let alias = root.join(NAMES[0]).join("unexpected-junction");
                    std::fs::create_dir(&alias).unwrap();
                    super::super::fixtures::junction(&outer, &alias, &target);
                }
                _ => unreachable!(),
            }
            assert!(matches!(cleanup.step(), CleanupProgress::Partial(_)));
            assert!(root
                .join(NAMES[0])
                .join("rollouts/synthetic-nested/nonsecret-fixture")
                .exists());
            assert!(root.join("lease").exists());
            if case == 3 {
                assert_eq!(
                    std::fs::read(outer.path().join("outside-owned-home/sentinel")).unwrap(),
                    b"untouched"
                );
            }
            drop(cleanup);
            if case == 3 {
                std::fs::remove_dir(root.join(NAMES[0]).join("unexpected-junction")).unwrap();
            }
            outer.close().unwrap();
        }
    }
    #[test]
    #[ignore = "owned synthetic real disposition interruption and custody recovery; no real data"]
    fn actual_partial_postorder_lease_root_and_receipt_interruptions_reconcile_without_replay() {
        for checkpoint in [
            Checkpoint::FirstNode,
            Checkpoint::LeaseRemoved,
            Checkpoint::RootRemoved,
            Checkpoint::CompletionSynced,
        ] {
            for reopen in [false, true] {
                let (outer, base, mut cleanup, journal, _) = fixture();
                let root = cleanup.root.clone();
                cleanup.stop = Some(checkpoint);
                assert_eq!(cleanup.step(), CleanupProgress::Interrupted);
                match checkpoint {
                    Checkpoint::FirstNode => {
                        assert!(cleanup.cursor.unwrap() < cleanup.nodes.len());
                        assert!(root.join("lease").exists());
                    }
                    Checkpoint::LeaseRemoved => {
                        assert!(root.is_dir());
                        assert!(!root.join("lease").exists());
                        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
                        assert_eq!(cleanup.phase(), Phase::Cleanup);
                    }
                    Checkpoint::RootRemoved => {
                        assert!(!root.exists());
                        assert_eq!(cleanup.phase(), Phase::RootRemoving);
                    }
                    Checkpoint::CompletionSynced => {
                        assert!(!root.exists());
                        assert_eq!(cleanup.phase(), Phase::Completed);
                    }
                    Checkpoint::ReceiptRemoved => unreachable!(),
                }
                if reopen {
                    drop(cleanup);
                    cleanup = recover(&base, journal.clone());
                } else {
                    cleanup
                        .retry(retry(&cleanup.intent, Arc::new(AtomicBool::new(false))))
                        .unwrap();
                }
                assert_eq!(cleanup.step(), CleanupProgress::Completed);
                assert!(!root.exists());
                assert!(intent_path(&base, cleanup.profile()).exists());
                journal.pending.store(false, Ordering::Release);
                cleanup.stop = Some(Checkpoint::ReceiptRemoved);
                assert_eq!(
                    cleanup.acknowledge_finalization(),
                    Err(RemovalFault::Interrupted)
                );
                assert!(!intent_path(&base, cleanup.profile()).exists());
                cleanup
                    .retry(retry(&cleanup.intent, Arc::new(AtomicBool::new(false))))
                    .unwrap();
                cleanup.acknowledge_finalization().unwrap();
                drop(cleanup);
                outer.close().unwrap();
            }
        }
    }
}
