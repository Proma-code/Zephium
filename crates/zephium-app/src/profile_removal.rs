//! Native participants in the existing, crash-resumable profile deletion.
//!
//! These ports report progress; they never grant filesystem or provider authority.
//! Concrete native owners retain their sealed same-lease cleanup capabilities.
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use zephium_core::ids::ProfileId;

/// Created only after the actor has validated an explicit deletion operation.
/// Clones retain the same process-local attempt, not a new cleanup permission.
#[derive(Clone)]
pub struct RemovalAttempt {
    profile: ProfileId,
    identity: Arc<()>,
}
impl RemovalAttempt {
    pub(crate) fn new(profile: ProfileId) -> Self {
        Self {
            profile,
            identity: Arc::new(()),
        }
    }
    pub fn profile(&self) -> ProfileId {
        self.profile
    }
    pub fn same_attempt(&self, other: &Self) -> bool {
        self.profile == other.profile && Arc::ptr_eq(&self.identity, &other.identity)
    }
}
/// The actor observed an exact canonical deletion journal row. This is not a
/// substitute for the native participant independently verifying that journal
/// and its own protected cleanup intent before promoting a held capability.
pub struct AuthorizedRemoval {
    attempt: RemovalAttempt,
}
impl AuthorizedRemoval {
    pub(crate) fn new(attempt: RemovalAttempt) -> Self {
        Self { attempt }
    }
    pub fn attempt(&self) -> &RemovalAttempt {
        &self.attempt
    }
    pub fn profile(&self) -> ProfileId {
        self.attempt.profile()
    }
}

/// A new explicit reconciliation request for the SAME already-admitted
/// deletion, not a new intent or permission to resend uncertain logout.
pub struct ExplicitRemovalRetry {
    attempt: RemovalAttempt,
}
/// Actual canonical finalization was confirmed, not merely enqueued. The
/// concrete owner must independently verify its protected completed receipt,
/// canonical cohort absence and fixed namespace before retiring that receipt.
pub struct FinalizedRemoval {
    attempt: RemovalAttempt,
}
impl FinalizedRemoval {
    pub(crate) fn new(attempt: RemovalAttempt) -> Self {
        Self { attempt }
    }
    pub fn attempt(&self) -> &RemovalAttempt {
        &self.attempt
    }
    pub fn profile(&self) -> ProfileId {
        self.attempt.profile()
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveredRemovalStage {
    Pending,
    /// Verified protected Completed plus canonical loaded pending/registered
    /// absence. Historical facts only; explicit retry supplies current action.
    FinalizationPending,
}
impl ExplicitRemovalRetry {
    pub(crate) fn new(attempt: RemovalAttempt) -> Self {
        Self { attempt }
    }
    pub fn attempt(&self) -> &RemovalAttempt {
        &self.attempt
    }
}
/// A verified protected pre-authorization intent retained by its native owner.
/// Enumeration establishes quarantine, not permission to continue deletion.
pub trait RecoverableRemoval: Send {
    fn profile(&self) -> ProfileId;
    fn stage(&self) -> RecoveredRemovalStage;
    fn retry_finalization(
        &mut self,
        retry: &ExplicitRemovalRetry,
        deadline: Instant,
    ) -> Preparation;
    /// Poll only an already admitted exact finalization action; no new fence.
    fn resume_finalization(
        &mut self,
        finalized: &FinalizedRemoval,
        deadline: Instant,
    ) -> Preparation;
    fn retry(&mut self, retry: &ExplicitRemovalRetry, deadline: Instant) -> Preparation;
    fn reconcile(&mut self, attempt: &RemovalAttempt, deadline: Instant) -> Preparation;
    fn resume_authorized(
        &mut self,
        authorization: &AuthorizedRemoval,
        deadline: Instant,
    ) -> Preparation;
    fn retry_authorized(
        &mut self,
        authorization: &AuthorizedRemoval,
        retry: &ExplicitRemovalRetry,
        deadline: Instant,
    ) -> Preparation;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovalStage {
    IntentPending,
    NativeIdle,
    LogoutPending,
    NativeSettlement,
    LocalCleanup,
    RecoveryRequired,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedKind {
    SignedOutSettled,
    /// Current canonical owner/mappings and this fixed managed namespace are
    /// absent. This is not a forensic history or credential-erasure claim.
    NotConfigured,
}
pub enum Preparation {
    /// Fresh admission refused before any native retirement, intent, or I/O.
    /// The caller must not quarantine the profile; a new explicit Delete may
    /// try again after the already-owned conversation has actually settled.
    Busy,
    /// Exact native attempt interruption; automatic timers must not reopen it.
    Interrupted(RemovalStage),
    Pending(RemovalStage),
    Blocked(RemovalStage),
    Prepared(Box<dyn PreparedRemoval>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupProgress {
    Verified,
    /// Retain the exact prepared custody and authorized continuation. Only an
    /// explicit retry can establish a fresh local cleanup action fence.
    Interrupted(RemovalStage),
    Pending(RemovalStage),
    Blocked(RemovalStage),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedStatus {
    Current,
    /// Same owned local preparation/renewal is in flight; polling never grants
    /// a new action or authorizes Store before the exact capability is current.
    Pending(RemovalStage),
    Interrupted(RemovalStage),
    Blocked(RemovalStage),
}
/// Move-only host object containing the concrete owner's cleanup capability.
/// Dropping it must never infer cleanup, revoke a replacement owner, or release
/// a still-active native lifecycle merely because the caller stopped waiting.
pub trait PreparedRemoval: Send {
    fn attempt(&self) -> &RemovalAttempt;
    fn kind(&self) -> PreparedKind;
    /// Recheck this exact custody/action immediately before every new Store
    /// authorization handoff. Historical preparation alone is not current.
    fn authorization_status(
        &mut self,
        attempt: &RemovalAttempt,
        deadline: Instant,
    ) -> PreparedStatus;
    /// Explicit local reconciliation on the SAME protected Prepared object;
    /// never recreates a provider logout or an ordinary active-profile grant.
    fn retry_before_authorization(
        &mut self,
        retry: &ExplicitRemovalRetry,
        deadline: Instant,
    ) -> PreparedStatus;
    fn cleanup_after_authorization(
        &mut self,
        authorization: &AuthorizedRemoval,
        deadline: Instant,
    ) -> CleanupProgress;
    /// Reconcile local cleanup on this SAME held object. This cannot restart
    /// provider logout, replace the intent, or revive its retired action fence.
    fn retry_cleanup_after_authorization(
        &mut self,
        authorization: &AuthorizedRemoval,
        retry: &ExplicitRemovalRetry,
        deadline: Instant,
    ) -> CleanupProgress;
    fn acknowledge_finalization(
        &mut self,
        finalized: &FinalizedRemoval,
        deadline: Instant,
    ) -> CleanupProgress;
    fn retry_after_finalization(
        &mut self,
        finalized: &FinalizedRemoval,
        retry: &ExplicitRemovalRetry,
        deadline: Instant,
    ) -> CleanupProgress;
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryStatus {
    Verified,
    Unavailable,
}
/// Explicit observational recheck, bounded to the actor's current attempts.
/// It neither creates deletion intent nor authorizes a prepared continuation.
pub struct InventoryRecheck {
    attempts: Vec<RemovalAttempt>,
}
impl InventoryRecheck {
    pub(crate) fn new(attempts: Vec<RemovalAttempt>) -> Self {
        Self { attempts }
    }
    pub fn attempts(&self) -> &[RemovalAttempt] {
        &self.attempts
    }
}
pub struct RecoveryInventory {
    pub status: InventoryStatus,
    /// Opaque individually verified records. An incomplete inventory may still
    /// report known affected profiles; it must never pretend the rest is empty.
    pub records: Vec<Box<dyn RecoverableRemoval>>,
}
pub trait ProfileRemovalParticipant: Send + Sync {
    /// Enumerate only verified protected intent records, bounded by the
    /// canonical profile cohort. Failure must not become an empty result.
    fn recover_pending(&self, deadline: Instant) -> RecoveryInventory;
    /// Before unsealing, verify the complete protected cohort and the identity
    /// of every already-known intent. A mismatch stays Unavailable. Rechecking
    /// must never reset native cancellation or start logout/auth/model work.
    fn recheck_inventory(&self, request: &InventoryRecheck, deadline: Instant)
        -> RecoveryInventory;
    /// Retirement only. The app invokes this before exposing an unprovable
    /// cohort; all native Codex admission/removal must remain sealed until a
    /// separately verified inventory reconciliation, never ordinary polling.
    fn seal_inventory(&self);
    fn retry(&self, retry: &ExplicitRemovalRetry, deadline: Instant) -> Preparation;
    /// Reconcile an interrupted, already-authorized cleanup only. This must
    /// not recreate an active-profile grant or revive a cancelled native fence.
    fn retry_authorized(
        &self,
        authorization: &AuthorizedRemoval,
        retry: &ExplicitRemovalRetry,
        deadline: Instant,
    ) -> Preparation;
    /// Confirm the protected intent and retire ordinary native admission, then
    /// settle original logout/read/process ownership while Store registration
    /// still exists. Retries retain the initial native deadline and never
    /// resend an uncertain provider request.
    fn prepare(&self, attempt: &RemovalAttempt, deadline: Instant) -> Preparation;
    /// Recover only from the participant's protected durable intent. Normal
    /// active-profile grant operations are unavailable after journal creation.
    fn resume_authorized(
        &self,
        authorization: &AuthorizedRemoval,
        deadline: Instant,
    ) -> Preparation;
}
static PARTICIPANT: OnceLock<Arc<dyn ProfileRemovalParticipant>> = OnceLock::new();
/// The composition root installs its sole owner before constructing the actor.
pub fn install(
    participant: Arc<dyn ProfileRemovalParticipant>,
) -> Result<(), Arc<dyn ProfileRemovalParticipant>> {
    PARTICIPANT.set(participant)
}
pub(crate) fn participant() -> Option<Arc<dyn ProfileRemovalParticipant>> {
    PARTICIPANT.get().cloned()
}
