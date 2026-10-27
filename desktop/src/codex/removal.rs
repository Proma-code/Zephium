//! Profile deletion joins the existing native owner and canonical app journal.
//! The app handles below are references to ONE retained custody slot, never
//! cloned native cleanup capabilities or renderer-supplied permissions.
use super::*;
use std::collections::BTreeMap;
use zephium_app::profile_removal as app;
use zephium_codex_broker::{
    connection::{RemovalAdmission, RemovalAttemptId, RemovalFailure},
    profile::{self as native, RemovalFault},
    removal::RemovalRetryAdmission,
};
use zephium_core::ports::store::{ProfileDeletionLoad, SessionLoad};

pub(super) struct Inventory {
    verified: AtomicBool,
    slots: Mutex<BTreeMap<ProfileId, Slot>>,
}
impl Default for Inventory {
    fn default() -> Self {
        Self {
            verified: AtomicBool::new(false),
            slots: Mutex::new(BTreeMap::new()),
        }
    }
}
pub(super) struct Operation {
    pub(super) profile: ProfileId,
    native_attempt: RemovalAttemptId,
    pub(super) cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    pub(super) deadline: Instant,
    begun: AtomicBool,
}
impl Operation {
    fn new(profile: ProfileId, attempt: RemovalAttemptId) -> Self {
        Self {
            profile,
            native_attempt: attempt,
            cancel: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(1)),
            deadline: Instant::now() + Duration::from_secs(120),
            begun: AtomicBool::new(false),
        }
    }
    pub(super) fn admission(&self) -> Option<RemovalAdmission> {
        RemovalAdmission::from_native(self.cancel.clone(), self.generation.clone(), 1)
    }
    pub(super) fn current(&self) -> bool {
        !self.cancel.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == 1
            && Instant::now() < self.deadline
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum LocalPhase {
    PrepareAbsent,
    Renew,
    Cleanup,
    Acknowledge,
}
pub(super) struct LocalRequest {
    profile: ProfileId,
    attempt: RemovalAttemptId,
    sequence: u64,
    phase: LocalPhase,
    retry: bool,
}
struct LocalReceipt {
    sequence: u64,
    phase: LocalPhase,
    result: Option<app::CleanupProgress>,
}
impl LocalReceipt {
    fn verified(&self, sequence: u64, phase: LocalPhase) -> bool {
        matches!(phase, LocalPhase::Cleanup | LocalPhase::Acknowledge)
            && self.sequence == sequence
            && self.phase == phase
            && self.result == Some(app::CleanupProgress::Verified)
    }
}
struct Slot {
    attempt: Option<app::RemovalAttempt>,
    native_attempt: RemovalAttemptId,
    action: Arc<Operation>,
    custody: Custody,
    finalization_pending: bool,
    local: Option<LocalReceipt>,
    sequence: u64,
}
enum Custody {
    Waiting,
    WaitingAbsent,
    Running,
    Prepared(Box<native::PreparedRemoval>),
    PreparedWriteFailed(Box<native::PreparationFailure>),
    FailedOwner(Box<RemovalFailure>),
    Recovered(Box<native::RecoveredRemoval>),
    Cleanup(Box<native::CleanupRemoval>),
    Absent(Box<native::NotConfiguredRemoval>),
    Blocked(RemovalFault),
    AbsentFailed(RemovalFault),
    Transition,
}
impl Custody {
    fn held(&mut self) -> Option<native::HeldRemoval<'_>> {
        match self {
            Self::Prepared(value) => Some(value.held_intent()),
            Self::PreparedWriteFailed(value) => Some(value.held_intent()),
            Self::FailedOwner(value) => value.held_intent(),
            Self::Recovered(value) => Some(value.held_intent()),
            Self::Cleanup(value) => value.held_intent(),
            Self::Absent(value) => value.held_intent(),
            _ => None,
        }
    }
    fn correlation(&self) -> Option<([u8; 16], u64)> {
        match self {
            Self::Prepared(v) => Some((v.incarnation(), v.retired_epoch())),
            Self::PreparedWriteFailed(v) => Some((v.incarnation(), v.retired_epoch())),
            Self::Recovered(v) => Some((v.incarnation()?, v.retired_epoch()?)),
            Self::Cleanup(v) => Some((v.incarnation(), v.retired_epoch())),
            _ => None,
        }
    }
}
struct Journal(Arc<zephium_store::SqliteStore>);
impl native::RemovalJournal for Journal {
    fn pending_profile_deletions(&self) -> ProfileDeletionLoad {
        self.0.pending_profile_deletions()
    }
    fn registered_profiles(&self) -> Option<Vec<ProfileId>> {
        match self.0.load_session() {
            SessionLoad::Loaded { state, .. }
            | SessionLoad::LoadedWithDegradedProfiles { state, .. } => Some(
                state
                    .profiles
                    .into_iter()
                    .map(|profile| profile.id)
                    .collect(),
            ),
            _ => None,
        }
    }
}
fn journal() -> Option<Arc<dyn native::RemovalJournal>> {
    Some(Arc::new(Journal(APP_STORE.get()?.clone())))
}
fn profile(id: ProfileId) -> Option<NativeProfile> {
    NativeProfile::from_native(id.bytes())
}
fn blocked() -> app::Preparation {
    app::Preparation::Blocked(app::RemovalStage::RecoveryRequired)
}
fn status(value: Result<(), RemovalFault>) -> app::PreparedStatus {
    match value {
        Ok(()) => app::PreparedStatus::Current,
        Err(RemovalFault::Interrupted) => {
            app::PreparedStatus::Interrupted(app::RemovalStage::LocalCleanup)
        }
        Err(_) => app::PreparedStatus::Blocked(app::RemovalStage::RecoveryRequired),
    }
}
fn progress(value: Result<(), RemovalFault>) -> app::CleanupProgress {
    match value {
        Ok(()) => app::CleanupProgress::Verified,
        Err(RemovalFault::Interrupted) => {
            app::CleanupProgress::Interrupted(app::RemovalStage::LocalCleanup)
        }
        Err(_) => app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired),
    }
}
impl Slot {
    fn preparation_pending(&self) -> bool {
        self.local.as_ref().is_some_and(|receipt| {
            matches!(receipt.phase, LocalPhase::PrepareAbsent | LocalPhase::Renew)
                && receipt.sequence == self.sequence
                && receipt.result.is_none()
        })
    }
    fn preparation_status(&self) -> Option<app::PreparedStatus> {
        let receipt = self.local.as_ref().filter(|receipt| {
            matches!(receipt.phase, LocalPhase::PrepareAbsent | LocalPhase::Renew)
                && receipt.sequence == self.sequence
        })?;
        match receipt.result {
            None | Some(app::CleanupProgress::Pending(_)) => Some(app::PreparedStatus::Pending(
                app::RemovalStage::LocalCleanup,
            )),
            Some(app::CleanupProgress::Interrupted(stage)) => {
                Some(app::PreparedStatus::Interrupted(stage))
            }
            Some(app::CleanupProgress::Blocked(stage)) => Some(app::PreparedStatus::Blocked(stage)),
            Some(app::CleanupProgress::Verified) => None, // must still validate the actual current capability
        }
    }
    fn bind(&mut self, attempt: &app::RemovalAttempt) -> bool {
        if attempt.profile() != self.action.profile {
            return false;
        }
        match self.attempt.as_ref() {
            Some(value) => value.same_attempt(attempt),
            None => {
                self.attempt = Some(attempt.clone());
                true
            }
        }
    }
    fn new_retry_action(&mut self) -> Result<(), RemovalFault> {
        let absence = matches!(&self.custody, Custody::Absent(_))
            || matches!(&self.custody,Custody::Recovered(value) if value.incarnation().is_none());
        if !absence && self.custody.correlation().is_none() {
            return Err(RemovalFault::RecoveryRequired);
        }
        // Explicit retry creates a distinct local-action fence. It never
        // resets the old cancelled fence held by historical provider evidence.
        self.action.cancel.store(true, Ordering::Release);
        self.action = Arc::new(Operation::new(self.action.profile, self.native_attempt));
        Ok(())
    }
    fn renew_native(&mut self) -> Result<(), RemovalFault> {
        let admission = self.action.admission().ok_or(RemovalFault::Scope)?;
        let id = profile(self.action.profile).ok_or(RemovalFault::Scope)?;
        if matches!(self.custody, Custody::Absent(_)) {
            let Custody::Absent(value) = &mut self.custody else {
                unreachable!()
            };
            return value.retry(id, self.native_attempt, admission);
        }
        if matches!(&self.custody, Custody::Recovered(v) if v.incarnation().is_none()) {
            let Custody::Recovered(value) =
                std::mem::replace(&mut self.custody, Custody::Transition)
            else {
                unreachable!()
            };
            let store = APP_STORE.get().cloned().ok_or(RemovalFault::Storage);
            let mut associations = match store {
                Ok(store) => DurableAssociations {
                    profile: self.action.profile,
                    store,
                },
                Err(error) => {
                    self.custody = Custody::Recovered(value);
                    return Err(error);
                }
            };
            return match value.reauthorize_absence(
                id,
                self.native_attempt,
                admission,
                Some(&mut associations),
            ) {
                Ok(value) => {
                    self.custody = Custody::Absent(Box::new(value));
                    Ok(())
                }
                Err(error) => {
                    let fault = error.fault();
                    let (value, _) = error.into_parts();
                    self.custody = Custody::Recovered(Box::new(value));
                    Err(fault)
                }
            };
        }
        let (incarnation, epoch) = self
            .custody
            .correlation()
            .ok_or(RemovalFault::RecoveryRequired)?;
        let retry = RemovalRetryAdmission::from_native_retry(
            id,
            self.native_attempt,
            incarnation,
            epoch,
            admission,
        )
        .map_err(|_| RemovalFault::Scope)?;
        let old = std::mem::replace(&mut self.custody, Custody::Transition);
        let result = match old {
            Custody::Prepared(value) => match value.retry(retry) {
                Ok(value) => {
                    self.custody = Custody::Prepared(Box::new(value));
                    Ok(())
                }
                Err(error) => {
                    let fault = error.fault();
                    let (value, _) = error.into_parts();
                    self.custody = Custody::Prepared(Box::new(value));
                    Err(fault)
                }
            },
            Custody::PreparedWriteFailed(value) => match value.retry(retry) {
                Ok(value) => {
                    self.custody = Custody::Prepared(Box::new(value));
                    Ok(())
                }
                Err(error) => {
                    let fault = error.fault();
                    let (value, _) = error.into_parts();
                    self.custody = Custody::PreparedWriteFailed(Box::new(value));
                    Err(fault)
                }
            },
            Custody::Recovered(value) => {
                if value.state() == native::RecoveryState::Prepared {
                    match value.reauthorize(retry) {
                        Ok(value) => {
                            self.custody = Custody::Prepared(Box::new(value));
                            Ok(())
                        }
                        Err(error) => {
                            let fault = error.fault();
                            let (value, _) = error.into_parts();
                            self.custody = Custody::Recovered(Box::new(value));
                            Err(fault)
                        }
                    }
                } else if let Some(journal) = journal() {
                    match value.resume_cleanup(retry, journal) {
                        Ok(value) => {
                            self.custody = Custody::Cleanup(Box::new(value));
                            Ok(())
                        }
                        Err(error) => {
                            let fault = error.fault();
                            let (value, _) = error.into_parts();
                            self.custody = Custody::Recovered(Box::new(value));
                            Err(fault)
                        }
                    }
                } else {
                    self.custody = Custody::Recovered(value);
                    Err(RemovalFault::Storage)
                }
            }
            Custody::Cleanup(mut value) => {
                let result = value.retry(retry);
                self.custody = Custody::Cleanup(value);
                result
            }
            other => {
                self.custody = other;
                Err(RemovalFault::RecoveryRequired)
            }
        };
        result
    }
    fn current(&mut self) -> Result<(), RemovalFault> {
        match &mut self.custody {
            Custody::Prepared(value) => value.check_current(),
            // Pure retained-action admission only. The worker independently
            // rechecks namespace and canonical journal before native cleanup
            // can be Verified (and therefore before any browser erasure).
            Custody::Absent(value) => value.check_admission(),
            Custody::Cleanup(value) => value.check_current(),
            _ => Err(RemovalFault::RecoveryRequired),
        }
    }
}

struct PreparedHandle {
    registry: std::sync::Weak<Registry>,
    attempt: app::RemovalAttempt,
    kind: app::PreparedKind,
}
impl PreparedHandle {
    fn with<T>(
        &self,
        attempt: &app::RemovalAttempt,
        f: impl FnOnce(&mut Slot) -> T,
    ) -> Result<T, RemovalFault> {
        if !self.attempt.same_attempt(attempt) {
            return Err(RemovalFault::Scope);
        }
        let registry = self.registry.upgrade().ok_or(RemovalFault::Interrupted)?;
        if registry.closed.load(Ordering::Acquire)
            || !registry.removals.verified.load(Ordering::Acquire)
        {
            return Err(RemovalFault::Interrupted);
        }
        let mut slots = registry
            .removals
            .slots
            .lock()
            .map_err(|_| RemovalFault::Storage)?;
        let slot = slots
            .get_mut(&attempt.profile())
            .ok_or(RemovalFault::Scope)?;
        if !slot.bind(attempt) {
            return Err(RemovalFault::Scope);
        }
        Ok(f(slot))
    }
    fn local(
        &self,
        attempt: &app::RemovalAttempt,
        deadline: Instant,
        retry: bool,
        finalized: bool,
    ) -> app::CleanupProgress {
        if Instant::now() >= deadline {
            return app::CleanupProgress::Pending(app::RemovalStage::LocalCleanup);
        }
        if !self.attempt.same_attempt(attempt) {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        }
        let Some(registry) = self.registry.upgrade() else {
            return app::CleanupProgress::Interrupted(app::RemovalStage::LocalCleanup);
        };
        registry.queue_removal_local(
            attempt,
            if finalized {
                LocalPhase::Acknowledge
            } else {
                LocalPhase::Cleanup
            },
            retry,
        )
    }
}
impl app::PreparedRemoval for PreparedHandle {
    fn attempt(&self) -> &app::RemovalAttempt {
        &self.attempt
    }
    fn kind(&self) -> app::PreparedKind {
        self.kind
    }
    fn authorization_status(
        &mut self,
        attempt: &app::RemovalAttempt,
        deadline: Instant,
    ) -> app::PreparedStatus {
        if Instant::now() >= deadline {
            return app::PreparedStatus::Interrupted(app::RemovalStage::LocalCleanup);
        }
        self.with(attempt, |slot| {
            slot.preparation_status()
                .unwrap_or_else(|| status(slot.current()))
        })
        .unwrap_or_else(|error| status(Err(error)))
    }
    fn retry_before_authorization(
        &mut self,
        retry: &app::ExplicitRemovalRetry,
        deadline: Instant,
    ) -> app::PreparedStatus {
        if Instant::now() >= deadline {
            return app::PreparedStatus::Interrupted(app::RemovalStage::LocalCleanup);
        }
        if !self.attempt.same_attempt(retry.attempt()) {
            return status(Err(RemovalFault::Scope));
        }
        let Some(registry) = self.registry.upgrade() else {
            return status(Err(RemovalFault::Interrupted));
        };
        match registry.queue_removal_local(retry.attempt(), LocalPhase::Renew, true) {
            app::CleanupProgress::Verified => self.authorization_status(retry.attempt(), deadline),
            app::CleanupProgress::Pending(stage) => app::PreparedStatus::Pending(stage),
            app::CleanupProgress::Interrupted(stage) => app::PreparedStatus::Interrupted(stage),
            app::CleanupProgress::Blocked(stage) => app::PreparedStatus::Blocked(stage),
        }
    }
    fn cleanup_after_authorization(
        &mut self,
        authorization: &app::AuthorizedRemoval,
        deadline: Instant,
    ) -> app::CleanupProgress {
        self.local(authorization.attempt(), deadline, false, false)
    }
    fn retry_cleanup_after_authorization(
        &mut self,
        authorization: &app::AuthorizedRemoval,
        retry: &app::ExplicitRemovalRetry,
        deadline: Instant,
    ) -> app::CleanupProgress {
        if !authorization.attempt().same_attempt(retry.attempt()) {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        }
        self.local(authorization.attempt(), deadline, true, false)
    }
    fn acknowledge_finalization(
        &mut self,
        finalized: &app::FinalizedRemoval,
        deadline: Instant,
    ) -> app::CleanupProgress {
        self.local(finalized.attempt(), deadline, false, true)
    }
    fn retry_after_finalization(
        &mut self,
        finalized: &app::FinalizedRemoval,
        retry: &app::ExplicitRemovalRetry,
        deadline: Instant,
    ) -> app::CleanupProgress {
        if !finalized.attempt().same_attempt(retry.attempt()) {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        }
        self.local(finalized.attempt(), deadline, true, true)
    }
}
fn queued_renewal(registry: &Arc<Registry>, attempt: &app::RemovalAttempt) -> app::Preparation {
    match registry.queue_removal_local(attempt, LocalPhase::Renew, true) {
        app::CleanupProgress::Pending(stage) => app::Preparation::Pending(stage),
        app::CleanupProgress::Interrupted(stage) => app::Preparation::Interrupted(stage),
        app::CleanupProgress::Blocked(stage) => app::Preparation::Blocked(stage),
        // Renewal isn't itself preparation authority. Poll the exact native
        // custody through prepared(), never infer a cap from a queue receipt.
        app::CleanupProgress::Verified => {
            app::Preparation::Pending(app::RemovalStage::LocalCleanup)
        }
    }
}
fn prepared(
    registry: &Arc<Registry>,
    attempt: &app::RemovalAttempt,
    slot: &mut Slot,
) -> app::Preparation {
    if !slot.bind(attempt) {
        return blocked();
    }
    if let Some(status) = slot.preparation_status() {
        return match status {
            app::PreparedStatus::Pending(stage) => app::Preparation::Pending(stage),
            app::PreparedStatus::Interrupted(stage) => app::Preparation::Interrupted(stage),
            app::PreparedStatus::Blocked(stage) => app::Preparation::Blocked(stage),
            app::PreparedStatus::Current => unreachable!("receipt cannot mint authority"),
        };
    }
    let kind = match &slot.custody {
        Custody::Absent(_) => app::PreparedKind::NotConfigured,
        Custody::Prepared(_) | Custody::Cleanup(_) => app::PreparedKind::SignedOutSettled,
        Custody::Waiting | Custody::WaitingAbsent => {
            return app::Preparation::Pending(app::RemovalStage::NativeIdle)
        }
        Custody::Running => return app::Preparation::Pending(app::RemovalStage::LogoutPending),
        Custody::Blocked(RemovalFault::Interrupted)
        | Custody::AbsentFailed(RemovalFault::Interrupted) => {
            return app::Preparation::Interrupted(app::RemovalStage::NativeSettlement)
        }
        Custody::PreparedWriteFailed(v) if v.fault() == RemovalFault::Interrupted => {
            return app::Preparation::Interrupted(app::RemovalStage::NativeSettlement)
        }
        _ => return blocked(),
    };
    match slot.current() {
        Ok(()) => app::Preparation::Prepared(Box::new(PreparedHandle {
            registry: Arc::downgrade(registry),
            attempt: attempt.clone(),
            kind,
        })),
        Err(RemovalFault::Interrupted) => {
            app::Preparation::Interrupted(app::RemovalStage::LocalCleanup)
        }
        Err(_) => blocked(),
    }
}
struct RecoveredHandle {
    registry: std::sync::Weak<Registry>,
    profile: ProfileId,
}
impl RecoveredHandle {
    fn prepare(
        &mut self,
        attempt: &app::RemovalAttempt,
        deadline: Instant,
        retry: bool,
        finalization: bool,
    ) -> app::Preparation {
        if attempt.profile() != self.profile {
            return blocked();
        }
        if Instant::now() >= deadline {
            return app::Preparation::Pending(app::RemovalStage::LocalCleanup);
        }
        let Some(registry) = self.registry.upgrade() else {
            return blocked();
        };
        if registry.closed.load(Ordering::Acquire)
            || !registry.removals.verified.load(Ordering::Acquire)
        {
            return blocked();
        }
        let Ok(mut slots) = registry.removals.slots.lock() else {
            return blocked();
        };
        let Some(slot) = slots.get_mut(&self.profile) else {
            return blocked();
        };
        if !slot.bind(attempt) || slot.finalization_pending != finalization {
            return blocked();
        }
        if retry {
            drop(slots);
            return queued_renewal(&registry, attempt);
        }
        prepared(&registry, attempt, slot)
    }
}
impl app::RecoverableRemoval for RecoveredHandle {
    fn profile(&self) -> ProfileId {
        self.profile
    }
    fn stage(&self) -> app::RecoveredRemovalStage {
        if self
            .registry
            .upgrade()
            .and_then(|r| {
                r.removals
                    .slots
                    .lock()
                    .ok()
                    .and_then(|slots| slots.get(&self.profile).map(|s| s.finalization_pending))
            })
            .unwrap_or(false)
        {
            app::RecoveredRemovalStage::FinalizationPending
        } else {
            app::RecoveredRemovalStage::Pending
        }
    }
    fn retry_finalization(
        &mut self,
        retry: &app::ExplicitRemovalRetry,
        deadline: Instant,
    ) -> app::Preparation {
        self.prepare(retry.attempt(), deadline, true, true)
    }
    fn resume_finalization(
        &mut self,
        finalized: &app::FinalizedRemoval,
        deadline: Instant,
    ) -> app::Preparation {
        self.prepare(finalized.attempt(), deadline, false, true)
    }
    fn retry(&mut self, retry: &app::ExplicitRemovalRetry, deadline: Instant) -> app::Preparation {
        self.prepare(retry.attempt(), deadline, true, false)
    }
    fn reconcile(&mut self, attempt: &app::RemovalAttempt, deadline: Instant) -> app::Preparation {
        self.prepare(attempt, deadline, false, false)
    }
    fn resume_authorized(
        &mut self,
        authorization: &app::AuthorizedRemoval,
        deadline: Instant,
    ) -> app::Preparation {
        self.prepare(authorization.attempt(), deadline, false, false)
    }
    fn retry_authorized(
        &mut self,
        authorization: &app::AuthorizedRemoval,
        retry: &app::ExplicitRemovalRetry,
        deadline: Instant,
    ) -> app::Preparation {
        if !authorization.attempt().same_attempt(retry.attempt()) {
            return blocked();
        }
        self.prepare(authorization.attempt(), deadline, true, false)
    }
}

pub(super) struct Participant {
    registry: std::sync::Weak<Registry>,
}
pub(super) fn install(registry: &Arc<Registry>) {
    if app::install(Arc::new(Participant {
        registry: Arc::downgrade(registry),
    }))
    .is_err()
    {
        registry.removals.verified.store(false, Ordering::Release);
        registry.closed.store(true, Ordering::Release);
    }
}
fn unavailable() -> app::RecoveryInventory {
    app::RecoveryInventory {
        status: app::InventoryStatus::Unavailable,
        records: vec![],
    }
}
impl Participant {
    fn inventory(
        &self,
        request: Option<&app::InventoryRecheck>,
        deadline: Instant,
    ) -> app::RecoveryInventory {
        let Some(registry) = self.registry.upgrade() else {
            return unavailable();
        };
        registry.removals.verified.store(false, Ordering::Release);
        // Revoke existing ordinary queued writer fences before exposing any
        // unprovable inventory. A foreground read cannot revive revision zero.
        let Ok(services) = registry.services.lock() else {
            return unavailable();
        };
        for service in services.values() {
            service.retire_ordinary();
        }
        drop(services);
        let Ok(mut slots) = registry.removals.slots.lock() else {
            return unavailable();
        };
        if let Some(request) = request {
            if request.attempts().iter().any(|attempt| {
                slots.get(&attempt.profile()).is_none_or(|slot| {
                    slot.attempt
                        .as_ref()
                        .is_none_or(|known| !known.same_attempt(attempt))
                })
            }) {
                return unavailable();
            }
        } else if !slots.is_empty() {
            return unavailable();
        }
        if slots.values().any(|s| {
            matches!(
                s.custody,
                Custody::Waiting | Custody::WaitingAbsent | Custody::Running | Custody::Transition
            )
        }) {
            return unavailable();
        }
        let mut held = slots
            .values_mut()
            .filter_map(|slot| slot.custody.held())
            .collect::<Vec<_>>();
        let observed = native::recheck_pending(&mut held, deadline);
        drop(held);
        let mut verified = observed.status == native::InventoryStatus::Verified;
        let mut records = Vec::<Box<dyn app::RecoverableRemoval>>::new();
        let Some(journal) = journal() else {
            return unavailable();
        };
        for mut record in observed.records {
            let id = ProfileId::from(u128::from_be_bytes(record.profile().bytes()));
            let native_attempt = record.attempt();
            if let Some(existing) = slots.get_mut(&id) {
                if matches!(existing.custody, Custody::AbsentFailed(_))
                    && record.incarnation().is_none()
                    && existing.native_attempt == native_attempt
                {
                    existing.custody = Custody::Recovered(Box::new(record));
                } else {
                    verified = false;
                }
                continue;
            }
            if slots.len() >= 256 {
                verified = false;
                continue;
            }
            let finalization_pending = if record.state() == native::RecoveryState::Completed {
                // A normal already-authorized deletion may still have its
                // canonical pending row. Only actual absence of BOTH cohorts
                // asks the native receipt to prove finalization-only recovery.
                match (
                    journal.pending_profile_deletions(),
                    journal.registered_profiles(),
                ) {
                    (ProfileDeletionLoad::Loaded(rows), Some(profiles))
                        if !rows.iter().any(|r| r.profile == id) && !profiles.contains(&id) =>
                    {
                        match record.finalization_pending(journal.as_ref()) {
                            Ok(true) => true,
                            _ => {
                                verified = false;
                                false
                            }
                        }
                    }
                    (ProfileDeletionLoad::Loaded(_), Some(_)) => false,
                    _ => {
                        verified = false;
                        false
                    }
                }
            } else {
                false
            };
            let action = Arc::new(Operation::new(id, native_attempt));
            action.cancel.store(true, Ordering::Release); // historical inventory is not action consent
            slots.insert(
                id,
                Slot {
                    attempt: None,
                    native_attempt,
                    action,
                    custody: Custody::Recovered(Box::new(record)),
                    finalization_pending,
                    local: None,
                    sequence: 0,
                },
            );
            records.push(Box::new(RecoveredHandle {
                registry: Arc::downgrade(&registry),
                profile: id,
            }));
        }
        if !verified {
            for slot in slots.values() {
                slot.action.cancel.store(true, Ordering::Release);
            }
        }
        registry
            .removals
            .verified
            .store(verified, Ordering::Release);
        app::RecoveryInventory {
            status: if verified {
                app::InventoryStatus::Verified
            } else {
                app::InventoryStatus::Unavailable
            },
            records,
        }
    }
    fn existing(
        &self,
        attempt: &app::RemovalAttempt,
        deadline: Instant,
        retry: bool,
    ) -> app::Preparation {
        if Instant::now() >= deadline {
            return app::Preparation::Pending(app::RemovalStage::LocalCleanup);
        }
        let Some(registry) = self.registry.upgrade() else {
            return blocked();
        };
        if registry.closed.load(Ordering::Acquire)
            || !registry.removals.verified.load(Ordering::Acquire)
        {
            return blocked();
        }
        let Ok(mut slots) = registry.removals.slots.lock() else {
            return blocked();
        };
        let Some(slot) = slots.get_mut(&attempt.profile()) else {
            return blocked();
        };
        if !slot.bind(attempt) {
            return blocked();
        }
        if retry {
            drop(slots);
            return queued_renewal(&registry, attempt);
        }
        prepared(&registry, attempt, slot)
    }
}
impl app::ProfileRemovalParticipant for Participant {
    fn recover_pending(&self, deadline: Instant) -> app::RecoveryInventory {
        self.inventory(None, deadline)
    }
    fn recheck_inventory(
        &self,
        request: &app::InventoryRecheck,
        deadline: Instant,
    ) -> app::RecoveryInventory {
        self.inventory(Some(request), deadline)
    }
    fn seal_inventory(&self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        registry.removals.verified.store(false, Ordering::Release);
        if let Ok(services) = registry.services.lock() {
            for service in services.values() {
                service.retire_ordinary();
            }
        }
        if let Ok(slots) = registry.removals.slots.lock() {
            for slot in slots.values() {
                slot.action.cancel.store(true, Ordering::Release);
            }
        }
    }
    fn retry(&self, retry: &app::ExplicitRemovalRetry, deadline: Instant) -> app::Preparation {
        self.existing(retry.attempt(), deadline, true)
    }
    fn retry_authorized(
        &self,
        authorization: &app::AuthorizedRemoval,
        retry: &app::ExplicitRemovalRetry,
        deadline: Instant,
    ) -> app::Preparation {
        if !authorization.attempt().same_attempt(retry.attempt()) {
            return blocked();
        }
        self.existing(authorization.attempt(), deadline, true)
    }
    fn prepare(&self, attempt: &app::RemovalAttempt, deadline: Instant) -> app::Preparation {
        let Some(registry) = self.registry.upgrade() else {
            return blocked();
        };
        if let Err(error) = registry.begin_removal_attempt(attempt, deadline) {
            return error;
        }
        self.existing(attempt, deadline, false)
    }
    fn resume_authorized(
        &self,
        authorization: &app::AuthorizedRemoval,
        deadline: Instant,
    ) -> app::Preparation {
        self.existing(authorization.attempt(), deadline, false)
    }
}

impl Inventory {
    pub(super) fn admission_open(&self) -> bool {
        self.verified.load(Ordering::Acquire)
    }
    pub(super) fn contains(&self, profile: ProfileId) -> bool {
        self.slots
            .lock()
            .map_or(true, |slots| slots.contains_key(&profile))
    }
}
impl Registry {
    fn begin_removal_attempt(
        self: &Arc<Self>,
        attempt: &app::RemovalAttempt,
        deadline: Instant,
    ) -> Result<(), app::Preparation> {
        if Instant::now() >= deadline {
            return Err(app::Preparation::Pending(app::RemovalStage::IntentPending));
        }
        if self.closed.load(Ordering::Acquire) || !self.removals.admission_open() {
            return Err(blocked());
        }
        // Same exclusion as ordinary native owner creation and direct Page CAS.
        let mut services = self.services.lock().map_err(|_| blocked())?;
        let mut slots = self.removals.slots.lock().map_err(|_| blocked())?;
        let id = attempt.profile();
        if let Some(slot) = slots.get_mut(&id) {
            if !slot.bind(attempt) {
                return Err(blocked());
            }
            if slot.preparation_pending()
                || !matches!(slot.custody, Custody::Waiting | Custody::WaitingAbsent)
            {
                return Ok(());
            }
        } else {
            if slots.len() >= 256 {
                return Err(blocked());
            }
            // A committed/reserved/active Page turn must settle before a new
            // removal can retire its original account admission. The same
            // queue gate closes reserve/handoff -> retirement atomically.
            let existing = services
                .get(&id)
                .filter(|service| !service.fully_settled())
                .cloned();
            if let Some(service) = existing {
                let result = service.queue.retire_if_idle(|| {
                    let mut selected = service
                        .removal
                        .lock()
                        .map_err(|_| admission::Error::Closed)?;
                    if selected.is_some() {
                        return Err(admission::Error::Closed);
                    }
                    let native_attempt =
                        RemovalAttemptId::fresh_native().map_err(|_| admission::Error::Closed)?;
                    let action = Arc::new(Operation::new(id, native_attempt));
                    slots.insert(
                        id,
                        Slot {
                            attempt: Some(attempt.clone()),
                            native_attempt,
                            action: action.clone(),
                            custody: Custody::Waiting,
                            finalization_pending: false,
                            local: None,
                            sequence: 0,
                        },
                    );
                    // A stopping owner may be physically unsettled even with
                    // no turn. Wait for it; never attach a new action to its
                    // already-closing Connection or revive its revision.
                    if !service.stop.load(Ordering::Acquire)
                        && !service.done.load(Ordering::Acquire)
                    {
                        *selected = Some(action);
                        service.account_revision.store(0, Ordering::Release);
                    }
                    Ok(())
                });
                match result {
                    Ok(()) => service.retire_ordinary(),
                    Err(admission::Error::Busy) => return Err(app::Preparation::Busy),
                    Err(admission::Error::Closed) => return Err(blocked()),
                }
            } else {
                let native_attempt = RemovalAttemptId::fresh_native().map_err(|_| blocked())?;
                slots.insert(
                    id,
                    Slot {
                        attempt: Some(attempt.clone()),
                        native_attempt,
                        action: Arc::new(Operation::new(id, native_attempt)),
                        custody: Custody::Waiting,
                        finalization_pending: false,
                        local: None,
                        sequence: 0,
                    },
                );
            }
        }
        let slot = slots.get_mut(&id).ok_or_else(blocked)?;
        if !slot.action.current() {
            slot.custody = Custody::Blocked(RemovalFault::Interrupted);
            return Ok(());
        }
        let existing = services
            .get(&id)
            .filter(|value| !value.fully_settled())
            .cloned();
        let mut absent = matches!(slot.custody, Custody::WaitingAbsent);
        let service = if let Some(service) = existing {
            if service.stop.load(Ordering::Acquire) || service.done.load(Ordering::Acquire) {
                return Err(app::Preparation::Pending(
                    app::RemovalStage::NativeSettlement,
                ));
            }
            let mut selected = service.removal.lock().map_err(|_| blocked())?;
            if selected
                .as_ref()
                .is_some_and(|known| !Arc::ptr_eq(known, &slot.action))
            {
                return Err(blocked());
            }
            *selected = Some(slot.action.clone());
            drop(selected);
            service.retire_ordinary();
            service
        } else {
            let store = APP_STORE.get().cloned().ok_or_else(blocked)?;
            let mut associations = DurableAssociations { profile: id, store };
            use zephium_codex_broker::connection::Associations as _;
            match associations.call(zephium_core::codex_store::Call::InspectOwner) {
                zephium_core::codex_store::Reply::Owner(None) => {
                    absent = true;
                    slot.custody = Custody::WaitingAbsent;
                }
                zephium_core::codex_store::Reply::Owner(Some(_)) => (),
                _ => {
                    slot.custody = Custody::Blocked(RemovalFault::RecoveryRequired);
                    return Ok(());
                }
            }
            services.retain(|_, value| !value.fully_settled());
            if services.len() >= 64 {
                return Err(app::Preparation::Pending(app::RemovalStage::NativeIdle));
            }
            self.spawn_service(
                &mut services,
                id,
                self.epoch.load(Ordering::Acquire),
                Some(slot.action.clone()),
                absent,
            )
            .map_err(|_| blocked())?
        };
        if absent {
            let sequence = slot.sequence.checked_add(1).ok_or_else(blocked)?;
            let request = LocalRequest {
                profile: id,
                attempt: slot.native_attempt,
                sequence,
                phase: LocalPhase::PrepareAbsent,
                retry: false,
            };
            return match service.enqueue(Command::RemovalLocal(request)) {
                Ok(()) => {
                    slot.sequence = sequence;
                    slot.local = Some(LocalReceipt {
                        sequence,
                        phase: LocalPhase::PrepareAbsent,
                        result: None,
                    });
                    Ok(())
                }
                Err(CodexFaultV1::Busy) => {
                    Err(app::Preparation::Pending(app::RemovalStage::IntentPending))
                }
                Err(_) => {
                    slot.custody = Custody::AbsentFailed(RemovalFault::RecoveryRequired);
                    Ok(())
                }
            };
        }
        match service.enqueue(Command::Removal(slot.action.clone())) {
            Ok(()) => {
                slot.custody = Custody::Running;
                Ok(())
            }
            Err(CodexFaultV1::Busy) => {
                Err(app::Preparation::Pending(app::RemovalStage::NativeIdle))
            }
            Err(_) => {
                slot.custody = Custody::Blocked(RemovalFault::RecoveryRequired);
                Ok(())
            }
        }
    }
}
pub(super) fn retire_active(service: &Service, active: &mut Option<Active>) {
    if service.removal.lock().is_ok_and(|value| value.is_some()) {
        service.retire_ordinary();
        if let Some(active) = active {
            active.call.cancel.store(true, Ordering::Release);
        }
    }
}
pub(super) fn finish(
    registry: &std::sync::Weak<Registry>,
    operation: &Arc<Operation>,
    connection: Connection,
) {
    let custody = match connection.prepare_removal() {
        Ok(settled) => match native::PreparedRemoval::from_settled(settled) {
            Ok(value) => Custody::Prepared(Box::new(value)),
            Err(error) => Custody::PreparedWriteFailed(Box::new(error)),
        },
        Err(error) => Custody::FailedOwner(Box::new(error)),
    };
    if let Some(registry) = registry.upgrade() {
        let mut slots = registry
            .removals
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots
            .get_mut(&operation.profile)
            .filter(|slot| Arc::ptr_eq(&slot.action, operation))
        {
            slot.custody = custody;
            return;
        }
        // An invariant failure cannot turn physical settlement into cleanup.
        // Preserve retained custody in the original profile slot if possible.
        registry.removals.verified.store(false, Ordering::Release);
        slots.entry(operation.profile).or_insert_with(|| Slot {
            attempt: None,
            native_attempt: operation.native_attempt,
            action: operation.clone(),
            custody,
            finalization_pending: false,
            local: None,
            sequence: 0,
        });
    }
}
pub(super) fn begin(
    operation: &Arc<Operation>,
    connection: &mut Connection,
    scope: &conversation::Scope,
) -> Result<bool, Fault> {
    if !operation.current() {
        return Err(Fault::Cancelled);
    }
    if operation.begun.load(Ordering::Acquire) {
        return Ok(true);
    }
    if !connection.can_begin_removal(scope) {
        return Ok(false);
    }
    if operation
        .begun
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(Fault::Protocol);
    }
    let admission = operation.admission().ok_or(Fault::ScopeChanged)?;
    connection.begin_removal(
        scope,
        operation.native_attempt,
        admission,
        operation.deadline,
    )?;
    Ok(true)
}
impl Operation {
    pub(super) fn begun(&self) -> bool {
        self.begun.load(Ordering::Acquire)
    }
}
impl Inventory {
    pub(super) fn cancel_all(&self) {
        self.verified.store(false, Ordering::Release);
        if let Ok(slots) = self.slots.lock() {
            for slot in slots.values() {
                slot.action.cancel.store(true, Ordering::Release);
            }
        }
    }
}
pub(super) fn open_failed(registry: &std::sync::Weak<Registry>, operation: &Arc<Operation>) {
    if let Some(registry) = registry.upgrade() {
        if let Ok(mut slots) = registry.removals.slots.lock() {
            if let Some(slot) = slots
                .get_mut(&operation.profile)
                .filter(|slot| Arc::ptr_eq(&slot.action, operation))
            {
                slot.custody = Custody::Blocked(RemovalFault::RecoveryRequired);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn operation() -> Arc<Operation> {
        Arc::new(Operation::new(
            ProfileId::from(1),
            RemovalAttemptId::fresh_native().unwrap(),
        ))
    }
    #[test]
    fn verified_local_receipt_is_phase_and_sequence_bound() {
        let receipt = LocalReceipt {
            sequence: 4,
            phase: LocalPhase::Acknowledge,
            result: Some(app::CleanupProgress::Verified),
        };
        assert!(receipt.verified(4, LocalPhase::Acknowledge));
        assert!(receipt.verified(4, LocalPhase::Acknowledge));
        assert!(!receipt.verified(3, LocalPhase::Acknowledge));
        assert!(!receipt.verified(5, LocalPhase::Acknowledge));
        assert!(!receipt.verified(4, LocalPhase::Cleanup));
        for phase in [LocalPhase::PrepareAbsent, LocalPhase::Renew] {
            let receipt = LocalReceipt { phase, ..receipt };
            assert!(!receipt.verified(4, phase));
        }
        for result in [
            None,
            Some(app::CleanupProgress::Pending(
                app::RemovalStage::LocalCleanup,
            )),
            Some(app::CleanupProgress::Interrupted(
                app::RemovalStage::LocalCleanup,
            )),
            Some(app::CleanupProgress::Blocked(
                app::RemovalStage::RecoveryRequired,
            )),
        ] {
            let receipt = LocalReceipt { result, ..receipt };
            assert!(!receipt.verified(4, LocalPhase::Acknowledge));
        }
    }
    #[test]
    fn preparation_receipt_never_substitutes_for_a_current_native_capability() {
        let action = operation();
        let mut slot = Slot {
            attempt: None,
            native_attempt: action.native_attempt,
            action,
            custody: Custody::WaitingAbsent,
            finalization_pending: false,
            local: Some(LocalReceipt {
                sequence: 1,
                phase: LocalPhase::PrepareAbsent,
                result: None,
            }),
            sequence: 1,
        };
        assert_eq!(
            slot.preparation_status(),
            Some(app::PreparedStatus::Pending(
                app::RemovalStage::LocalCleanup
            ))
        );
        slot.local.as_mut().unwrap().result = Some(app::CleanupProgress::Verified);
        assert_eq!(slot.preparation_status(), None);
        assert_eq!(slot.current(), Err(RemovalFault::RecoveryRequired));
        slot.local.as_mut().unwrap().result = Some(app::CleanupProgress::Interrupted(
            app::RemovalStage::LocalCleanup,
        ));
        assert_eq!(
            slot.preparation_status(),
            Some(app::PreparedStatus::Interrupted(
                app::RemovalStage::LocalCleanup
            ))
        );
        assert!(!slot
            .local
            .as_ref()
            .unwrap()
            .verified(1, LocalPhase::PrepareAbsent));
    }
    #[test]
    fn initial_inventory_is_sealed_and_contains_no_authority() {
        let inventory = Inventory::default();
        assert!(!inventory.admission_open());
        assert!(!inventory.contains(ProfileId::from(1)));
    }
    #[test]
    fn observation_never_renews_the_original_absolute_deadline() {
        let operation = operation();
        let deadline = operation.deadline;
        assert!(operation.current());
        assert!(operation.current());
        assert_eq!(operation.deadline, deadline);
        operation.cancel.store(true, Ordering::Release);
        assert!(!operation.current());
        assert_eq!(operation.deadline, deadline);
    }
    #[test]
    fn explicit_retry_cannot_rearm_an_unsatisfied_running_obligation() {
        let action = operation();
        let mut slot = Slot {
            attempt: None,
            native_attempt: action.native_attempt,
            action: action.clone(),
            custody: Custody::Running,
            finalization_pending: false,
            local: None,
            sequence: 0,
        };
        assert_eq!(slot.new_retry_action(), Err(RemovalFault::RecoveryRequired));
        assert!(Arc::ptr_eq(&action, &slot.action));
        assert!(action.current());
        assert!(matches!(slot.custody, Custody::Running));
    }
    #[test]
    fn shutdown_retires_retained_actions_even_without_a_live_service() {
        let inventory = Inventory::default();
        let action = operation();
        inventory.slots.lock().unwrap().insert(
            action.profile,
            Slot {
                attempt: None,
                native_attempt: action.native_attempt,
                action: action.clone(),
                custody: Custody::Waiting,
                finalization_pending: false,
                local: None,
                sequence: 0,
            },
        );
        inventory.verified.store(true, Ordering::Release);
        inventory.cancel_all();
        assert!(!inventory.admission_open());
        assert!(!action.current());
        assert!(inventory.contains(action.profile));
    }
}

#[cfg(test)]
pub(super) fn test_operation(profile: ProfileId) -> Arc<Operation> {
    Arc::new(Operation::new(
        profile,
        RemovalAttemptId::fresh_native().unwrap(),
    ))
}
impl Registry {
    fn queue_removal_local(
        self: &Arc<Self>,
        attempt: &app::RemovalAttempt,
        phase: LocalPhase,
        retry: bool,
    ) -> app::CleanupProgress {
        let pending = app::CleanupProgress::Pending(app::RemovalStage::LocalCleanup);
        // A recorded success is a historical result, not fresh action authority.
        // Deliver only the same bound attempt, latest local sequence and phase,
        // even after retirement or an explicit retry. Never reissue that effect.
        if self.removals.slots.lock().is_ok_and(|slots| {
            slots.get(&attempt.profile()).is_some_and(|slot| {
                slot.attempt
                    .as_ref()
                    .is_some_and(|bound| bound.same_attempt(attempt))
                    && slot
                        .local
                        .as_ref()
                        .is_some_and(|receipt| receipt.verified(slot.sequence, phase))
            })
        }) {
            return app::CleanupProgress::Verified;
        }
        if self.closed.load(Ordering::Acquire) || !self.removals.admission_open() {
            return app::CleanupProgress::Interrupted(app::RemovalStage::LocalCleanup);
        }
        let Ok(mut services) = self.services.lock() else {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        };
        let Ok(mut slots) = self.removals.slots.lock() else {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        };
        let Some(slot) = slots.get_mut(&attempt.profile()) else {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        };
        if !slot.bind(attempt) {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        }
        if let Some(receipt) = slot.local.as_ref() {
            if receipt.result.is_none() {
                return pending;
            }
            if receipt.verified(slot.sequence, phase) || (!retry && receipt.phase == phase) {
                return receipt.result.unwrap_or(pending);
            }
        }
        let service = if let Some(service) = services
            .get(&attempt.profile())
            .filter(|service| !service.fully_settled())
        {
            if service.stop.load(Ordering::Acquire) || service.done.load(Ordering::Acquire) {
                return pending;
            }
            service.clone()
        } else {
            services.retain(|_, service| !service.fully_settled());
            if services.len() >= 64 {
                return pending;
            }
            match self.spawn_service(
                &mut services,
                attempt.profile(),
                self.epoch.load(Ordering::Acquire),
                Some(slot.action.clone()),
                true,
            ) {
                Ok(service) => service,
                Err(_) => {
                    return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired)
                }
            }
        };
        let Some(sequence) = slot.sequence.checked_add(1) else {
            return app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired);
        };
        let request = LocalRequest {
            profile: attempt.profile(),
            attempt: slot.native_attempt,
            sequence,
            phase,
            retry,
        };
        match service.enqueue(Command::RemovalLocal(request)) {
            Ok(()) => {
                slot.sequence = sequence;
                slot.local = Some(LocalReceipt {
                    sequence,
                    phase,
                    result: None,
                });
                pending
            }
            Err(CodexFaultV1::Busy) => pending,
            Err(_) => app::CleanupProgress::Blocked(app::RemovalStage::RecoveryRequired),
        }
    }
}
fn execute_local(registry: &Registry, request: LocalRequest) -> bool {
    let mut slots = registry
        .removals
        .slots
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(slot) = slots.get_mut(&request.profile).filter(|slot| {
        slot.native_attempt == request.attempt
            && slot.local.as_ref().is_some_and(|receipt| {
                receipt.sequence == request.sequence
                    && receipt.phase == request.phase
                    && receipt.result.is_none()
            })
    }) else {
        return false;
    };
    if request.retry {
        if let Err(error) = slot.new_retry_action() {
            if let Some(receipt) = slot.local.as_mut() {
                receipt.result = Some(progress(Err(error)));
            }
            return false;
        }
    }
    // The exact capability moves into this same worker while I/O runs. The
    // coordinator sees a Pending receipt, never waits behind filesystem work.
    // Its action Arc remains in the registry so shutdown/inventory retirement
    // can synchronously revoke even this in-flight local operation.
    let mut owned = Slot {
        attempt: slot.attempt.clone(),
        native_attempt: slot.native_attempt,
        action: slot.action.clone(),
        custody: std::mem::replace(&mut slot.custody, Custody::Transition),
        finalization_pending: slot.finalization_pending,
        local: None,
        sequence: slot.sequence,
    };
    drop(slots);
    let slot = &mut owned;
    let result = (|| -> Result<(), RemovalFault> {
        if registry.closed.load(Ordering::Acquire) || !registry.removals.admission_open() {
            return Err(RemovalFault::Interrupted);
        }
        if request.phase == LocalPhase::PrepareAbsent {
            if !matches!(slot.custody, Custody::WaitingAbsent) || !slot.action.current() {
                return Err(RemovalFault::Interrupted);
            }
            let result = (|| {
                let admission = slot.action.admission().ok_or(RemovalFault::Scope)?;
                let id = profile(request.profile).ok_or(RemovalFault::Scope)?;
                let store = APP_STORE.get().cloned().ok_or(RemovalFault::Storage)?;
                let mut associations = DurableAssociations {
                    profile: request.profile,
                    store,
                };
                native::NotConfiguredRemoval::from_native(
                    id,
                    request.attempt,
                    admission,
                    &mut associations,
                )
            })();
            return match result {
                Ok(value) => {
                    slot.custody = Custody::Absent(Box::new(value));
                    Ok(())
                }
                Err(error) => {
                    slot.custody = Custody::AbsentFailed(error);
                    Err(error)
                }
            };
        }
        if request.retry {
            slot.renew_native()?;
        }
        if request.phase == LocalPhase::Renew {
            return slot.current();
        }
        let journal = journal().ok_or(RemovalFault::Storage)?;
        if request.phase == LocalPhase::Acknowledge {
            return match &mut slot.custody {
                Custody::Absent(value) => value.acknowledge_finalization(journal.as_ref()),
                Custody::Cleanup(value) => value.acknowledge_finalization(),
                _ => Err(RemovalFault::Scope),
            };
        }
        if matches!(slot.custody, Custody::Prepared(_)) {
            let Custody::Prepared(value) =
                std::mem::replace(&mut slot.custody, Custody::Transition)
            else {
                unreachable!()
            };
            slot.custody = Custody::Cleanup(Box::new(value.into_cleanup(journal.clone())));
        }
        match &mut slot.custody {
            Custody::Absent(value) => value.verify_after_authorization(journal.as_ref()),
            Custody::Cleanup(value) => match value.step() {
                native::CleanupProgress::Completed => Ok(()),
                native::CleanupProgress::Interrupted => Err(RemovalFault::Interrupted),
                native::CleanupProgress::Partial(error) => Err(error),
            },
            _ => Err(RemovalFault::Scope),
        }
    })();
    let complete = request.phase == LocalPhase::Acknowledge && result.is_ok();
    let mut slots = registry
        .removals
        .slots
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(slot) = slots.get_mut(&request.profile).filter(|slot| {
        slot.native_attempt == request.attempt
            && slot.sequence == request.sequence
            && matches!(slot.custody, Custody::Transition)
    }) else {
        registry.removals.verified.store(false, Ordering::Release);
        return false;
    };
    slot.custody = owned.custody;
    if let Some(receipt) = slot.local.as_mut() {
        receipt.result = Some(progress(result));
    }
    complete
}
/// Same existing worker/queue, after the original process has physically
/// settled (or for a proven absent home, without ever launching a process).
pub(super) fn local_worker(
    service: &Service,
    registry: &std::sync::Weak<Registry>,
    receiver: admission::Receiver<Command>,
) {
    service.retire_ordinary();
    set_fault(service, CodexFaultV1::Unavailable);
    loop {
        let Some(registry) = registry.upgrade() else {
            break;
        };
        if service.stop.load(Ordering::Acquire) || registry.closed.load(Ordering::Acquire) {
            break;
        }
        match receiver.try_recv() {
            Ok(Command::RemovalLocal(request)) if request.profile == service.profile => {
                if execute_local(&registry, request) {
                    break;
                }
            }
            Ok(Command::Select(call)) => {
                let _ = call.result.send(Err(CodexFaultV1::ScopeChanged));
            }
            Ok(Command::Reply(call)) => {
                let _ = call.result.send(ReplyDelivery::Failed(
                    PageAssistNativeFaultV1::OutcomeUnknown,
                ));
            }
            Ok(_) => (),
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    service.stop.store(true, Ordering::Release);
}

pub(super) fn read_account(
    service: &Service,
    connection: &mut Connection,
    scope: &conversation::Scope,
) -> Result<(), Fault> {
    if let Some(operation) = service.removal.lock().ok().and_then(|value| value.clone()) {
        let admission = operation.admission().ok_or(Fault::ScopeChanged)?;
        connection.read_account_for_removal(scope, &admission, operation.deadline)
    } else {
        connection.read_account(scope, &service.stop)
    }
}
pub(super) fn cancel_login(
    service: &Service,
    connection: &mut Connection,
    scope: &conversation::Scope,
) -> Result<(), Fault> {
    if let Some(operation) = service.removal.lock().ok().and_then(|value| value.clone()) {
        let admission = operation.admission().ok_or(Fault::ScopeChanged)?;
        connection.cancel_login_for_removal(scope, &admission, operation.deadline)
    } else {
        connection.cancel_login(scope, &service.stop)
    }
}

/// Join the removal action before any ordinary handler or pending reply is pumped.
pub(super) fn attach_preparation(
    service: &Service,
    connection: &mut Connection,
    scope: &conversation::Scope,
) -> Result<(), Fault> {
    if let Some(operation) = service.removal.lock().ok().and_then(|value| value.clone()) {
        // Protected removal already retains this exact fence; setup must not
        // re-enter ordinary admission after begin has advanced the grant.
        if !operation.begun.load(Ordering::Acquire) {
            let admission = operation.admission().ok_or(Fault::ScopeChanged)?;
            connection.attach_removal_preparation(scope, &admission, operation.deadline)?;
        }
    }
    Ok(())
}
pub(super) fn cancel_turn(
    service: &Service,
    connection: &mut Connection,
    scope: &conversation::Scope,
) -> Result<(), Fault> {
    if let Some(operation) = service.removal.lock().ok().and_then(|value| value.clone()) {
        let admission = operation.admission().ok_or(Fault::ScopeChanged)?;
        connection.cancel_turn_for_removal(scope, &admission, operation.deadline)
    } else {
        connection.cancel_turn(scope, &service.stop)
    }
}
