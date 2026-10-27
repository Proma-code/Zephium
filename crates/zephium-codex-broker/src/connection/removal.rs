//! Explicit native removal obligation. Ordinary close, account observations and
//! transport cleanup cannot construct this same-lease settlement capability.
use super::*;
use crate::{
    profile::{native::removal::RemovalReservation, removal::RemovalAttemptId},
    removal::{CredentialObservation, RemovalAdmission, RemovalIntent},
};

/// Read/cancel-only setup for one already-admitted native deletion action.
/// This is not Protected Reserved or logout/grant authority.
pub(super) struct AccountPreparation {
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) fence: windows::WriteFence,
    pub(super) deadline: Instant,
    requested_deadline: Instant,
}
impl AccountPreparation {
    pub(super) fn new(admission: &RemovalAdmission, deadline: Instant) -> Result<Self, Fault> {
        admission.current().map_err(removal_fault)?;
        let value = Self {
            cancel: admission.cancel_flag(),
            fence: admission.writer_fence(),
            deadline: deadline.min(Instant::now() + PREPARE),
            requested_deadline: deadline,
        };
        value.current()?;
        Ok(value)
    }
    pub(super) fn current(&self) -> Result<(), Fault> {
        check(self.deadline, &self.cancel)?;
        if !self.fence.current() {
            return Err(Fault::ScopeChanged);
        }
        Ok(())
    }
    fn joins(&self, admission: &RemovalAdmission, deadline: Instant) -> Result<(), Fault> {
        self.current()?;
        admission.current().map_err(removal_fault)?;
        if self.requested_deadline != deadline || !self.fence.same_action(&admission.writer_fence())
        {
            return Err(Fault::ScopeChanged);
        }
        Ok(())
    }
}

pub(super) struct RemovalState {
    pub(super) intent: RemovalIntent,
    reservation: RemovalReservation,
    attempt: RemovalAttemptId,
    pub(super) deadline: Instant,
}

/// A failed attempt keeps the original connection/lease/reservation custody.
/// Historical logout observations are not a filesystem cleanup capability.
pub struct RemovalFailure {
    fault: Fault,
    owner: Box<Connection>,
}
impl RemovalFailure {
    pub fn fault(&self) -> Fault {
        self.fault
    }
    pub fn observation(&self) -> CredentialObservation {
        self.owner.removal_observation()
    }
    pub fn held_intent(&mut self) -> Option<crate::profile::HeldRemoval<'_>> {
        self.owner
            .removal
            .as_mut()
            .map(|state| state.reservation.held_intent())
    }
}

/// Only Connection::prepare_removal can join original logout confirmation,
/// exact physical process settlement and the original persistent lease.
/// This is not yet Store deletion authorization or proof of remote revocation.
pub struct SettledRemoval {
    lease: ProfileLease,
    reservation: RemovalReservation,
    evidence: RemovalEvidence,
}
/// Retained lifetime fence travels with the same-lease capability into protected
/// Prepared/tombstone publication. It cannot be replaced by a historical bit.
pub(crate) struct RemovalEvidence {
    intent: RemovalIntent,
    scope: Scope,
    canonical: durable::Scope,
    attempt: RemovalAttemptId,
}
impl RemovalEvidence {
    pub(crate) fn check_current(&self) -> Result<(), Fault> {
        self.intent
            .observed(
                &self.scope,
                self.canonical.incarnation,
                self.canonical.grant_epoch,
            )
            .map_err(removal_fault)
    }
    pub(crate) fn retired_epoch(&self) -> u64 {
        self.canonical.grant_epoch
    }
    pub(crate) fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }
    pub(crate) fn incarnation(&self) -> [u8; 16] {
        self.canonical.incarnation
    }
}
impl SettledRemoval {
    pub fn profile(&self) -> ProfileId {
        self.lease.record.profile
    }
    pub fn attempt(&self) -> RemovalAttemptId {
        self.evidence.attempt()
    }
    pub fn incarnation(&self) -> [u8; 16] {
        self.evidence.incarnation()
    }
    pub fn retired_epoch(&self) -> u64 {
        self.evidence.retired_epoch()
    }
    pub fn held_intent(&mut self) -> crate::profile::HeldRemoval<'_> {
        self.reservation.held_intent()
    }
    pub(crate) fn check_current(&self) -> Result<(), Fault> {
        self.evidence.check_current()
    }
    pub(crate) fn into_parts(self) -> (ProfileLease, RemovalReservation, RemovalEvidence) {
        (self.lease, self.reservation, self.evidence)
    }
    pub(crate) fn from_parts(
        lease: ProfileLease,
        reservation: RemovalReservation,
        evidence: RemovalEvidence,
    ) -> Self {
        // Only the profile Prepared constructor may reconstruct the *same*
        // consumed parts on failure. Evidence has no independent constructor.
        Self {
            lease,
            reservation,
            evidence,
        }
    }
    pub fn observation(&self) -> CredentialObservation {
        self.evidence.intent.observation()
    }
}

impl Connection {
    pub(super) fn ordinary_current(&self, scope: &Scope) -> Result<(), Fault> {
        self.current(scope)?;
        if self.removal.is_some() || self.removal_setup.is_some() {
            Err(Fault::Busy)
        } else {
            Ok(())
        }
    }
    fn removal_preflight(&self, scope: &Scope) -> Result<(), Fault> {
        self.current(scope)?;
        if self.wake_deadline.is_some() {
            return Err(Fault::Busy);
        }
        if self.removal.is_some() {
            return Err(Fault::Busy);
        }
        if let Some(setup) = &self.removal_setup {
            setup.current()?;
        }
        self.idle()?;
        self.account.removal_ready(scope).map_err(managed_fault)
    }
    fn removal_account_setup(
        &mut self,
        scope: &Scope,
        admission: &RemovalAdmission,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.current(scope)?;
        if self.removal.is_some() {
            return Err(Fault::Busy);
        }
        if let Some(setup) = &self.removal_setup {
            setup.joins(admission, deadline)?;
        } else {
            self.removal_setup = Some(AccountPreparation::new(admission, deadline)?);
        }
        Ok(())
    }
    /// Attach immediately when the native worker takes this lifecycle action.
    /// Already-owned observational replies drain, but cannot schedule new work.
    /// This does not reserve filesystem intent or authorize logout/grant change.
    pub fn attach_removal_preparation(
        &mut self,
        scope: &Scope,
        admission: &RemovalAdmission,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.removal_account_setup(scope, admission, deadline)?;
        self.catalog = None;
        self.catalog_builder = None;
        Ok(())
    }
    /// Existing fixed account/read only, under the original removal action.
    pub fn read_account_for_removal(
        &mut self,
        scope: &Scope,
        admission: &RemovalAdmission,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.removal_account_setup(scope, admission, deadline)?;
        let setup = self.removal_setup.as_ref().ok_or(Fault::Protocol)?;
        let cancel = setup.cancel.clone();
        self.read_account_until(scope, &cancel, setup.deadline.min(Instant::now() + RPC))
    }
    /// Cancel only the original login ceremony. A deferred exact-login-ID
    /// cancellation retains this action/deadline through the later callback.
    pub fn cancel_login_for_removal(
        &mut self,
        scope: &Scope,
        admission: &RemovalAdmission,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.removal_account_setup(scope, admission, deadline)?;
        let setup = self.removal_setup.as_ref().ok_or(Fault::Protocol)?;
        let cancel = setup.cancel.clone();
        self.account_deadline = Some(setup.deadline.min(Instant::now() + RPC));
        let event = self.account.cancel_login(scope).map_err(managed_fault)?;
        self.account_effect(event, &cancel, &mut Vec::new())
    }
    /// Retire only the existing turn through its original interrupt/Closed path.
    pub fn cancel_turn_for_removal(
        &mut self,
        scope: &Scope,
        admission: &RemovalAdmission,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.removal_account_setup(scope, admission, deadline)?;
        let cancel = self
            .removal_setup
            .as_ref()
            .ok_or(Fault::Protocol)?
            .cancel
            .clone();
        self.cancel_existing_turn(scope, &cancel)
    }
    /// Observational only: no attempt, reservation, logout or cleanup authority.
    /// The consuming native action must still pass the same preflight in begin.
    pub fn can_begin_removal(&self, scope: &Scope) -> bool {
        self.removal_preflight(scope).is_ok()
    }
    /// Invoke only for the current explicit app actor attempt. The protected
    /// reservation is flushed before the ordinary grant retirement or logout.
    /// Retrying a preparation keeps this obligation, never invokes this again.
    pub fn begin_removal(
        &mut self,
        scope: &Scope,
        attempt: RemovalAttemptId,
        admission: RemovalAdmission,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.removal_preflight(scope)?;
        if let Some(setup) = &self.removal_setup {
            setup.joins(&admission, deadline)?;
        }
        admission.current().map_err(removal_fault)?;
        let cancel = admission.cancel_flag();
        let deadline = deadline.min(Instant::now() + PREPARE).min(self.deadline);
        let deadline = self
            .removal_setup
            .as_ref()
            .map_or(deadline, |setup| deadline.min(setup.deadline));
        check(deadline, &cancel)?;
        let intent = RemovalIntent::new(
            scope.clone(),
            self.association_scope.incarnation,
            self.association_scope.grant_epoch,
            admission,
        )
        .map_err(removal_fault)?;
        let reservation = match self
            .runtime
            .as_ref()
            .ok_or(Fault::Closed)?
            .reserve_removal(attempt, self.association_scope.grant_epoch)
        {
            Ok(value) => value,
            Err(_) => return self.fail(Fault::PersistenceUnavailable),
        };
        self.removal_setup = None;
        self.removal = Some(RemovalState {
            intent,
            reservation,
            attempt,
            deadline,
        });
        // Protected CREATE_NEW/sync may block. Retain the exact reservation on
        // cancellation, but never retire a grant using the pre-I/O observation.
        self.removal
            .as_ref()
            .ok_or(Fault::Protocol)?
            .intent
            .current()
            .map_err(removal_fault)?;
        self.catalog = None;
        self.catalog_builder = None;
        self.account_deadline = Some(deadline);
        let event = self
            .account
            .logout_for_removal(LogoutAction::from_native_action(scope.clone()))
            .map_err(managed_fault)?;
        self.account_effect(event, &cancel, &mut Vec::new())
            .or_else(|fault| self.fail(fault))
    }
    pub fn removal_observation(&self) -> CredentialObservation {
        self.removal
            .as_ref()
            .map_or(CredentialObservation::Unconfirmed, |state| {
                state.intent.observation()
            })
    }
    pub fn removal_ready(&self, scope: &Scope) -> bool {
        self.current(scope).is_ok()
            && self.idle().is_ok()
            // Observations do not block turns, but every already-owned reply
            // must drain before strict EOF can certify removal settlement.
            && self.pending.is_empty()
            && self.state() == AccountState::SignedOut
            && self.removal.as_ref().is_some_and(|state| {
                Instant::now() < state.deadline
                    && state
                        .intent
                        .observed(
                            scope,
                            self.association_scope.incarnation,
                            self.association_scope.grant_epoch,
                        )
                        .is_ok()
            })
    }
    /// Consumes this original owner; no path reopen, ordinary Drop, forced kill,
    /// timeout, logout ACK alone or stale read can produce a successful receipt.
    pub fn prepare_removal(mut self) -> Result<SettledRemoval, RemovalFailure> {
        let result = self.prepare_removal_inner();
        match result {
            Ok(value) => Ok(value),
            Err(fault) => {
                // Physical teardown does not upgrade failed logout/readback or
                // publication into graceful settlement or cleanup authority.
                // Retain the same runtime/lease/intent after the child drains.
                let _: Result<(), Fault> = self.fail(fault);
                Err(RemovalFailure {
                    fault,
                    owner: Box::new(self),
                })
            }
        }
    }
    fn prepare_removal_inner(&mut self) -> Result<SettledRemoval, Fault> {
        self.current(&self.scope)?;
        self.idle()?;
        if !self.pending.is_empty() {
            return Err(Fault::Busy);
        }
        let state = self.removal.as_ref().ok_or(Fault::Protocol)?;
        state.intent.current().map_err(removal_fault)?;
        if self.state() != AccountState::SignedOut || Instant::now() >= state.deadline {
            return Err(if Instant::now() >= state.deadline {
                Fault::Timeout
            } else {
                Fault::Busy
            });
        }
        state
            .intent
            .observed(
                &self.scope,
                self.association_scope.incarnation,
                self.association_scope.grant_epoch,
            )
            .map_err(removal_fault)?;
        let deadline = state.deadline.min(Instant::now() + CLOSE);
        // finish requires actual zero-exit, empty Job, writer join and both EOFs.
        // It rejects any unread late frame, including a later account/updated.
        let session = self.session.take().ok_or(Fault::Closed)?;
        let exit = session.finish(deadline).map_err(transport)?;
        if exit.code != 0 {
            return Err(Fault::Transport);
        }
        let state = self.removal.as_ref().ok_or(Fault::Protocol)?;
        if Instant::now() >= state.deadline {
            return Err(Fault::Timeout);
        }
        state
            .intent
            .observed(
                &self.scope,
                self.association_scope.incarnation,
                self.association_scope.grant_epoch,
            )
            .map_err(removal_fault)?;
        let runtime = self.runtime.take().ok_or(Fault::Closed)?;
        let runtime = match Arc::try_unwrap(runtime) {
            Ok(value) => value,
            Err(value) => {
                self.runtime = Some(value);
                return Err(Fault::Transport);
            }
        };
        let lease = match runtime.into_removal() {
            Ok(value) => value,
            Err(value) => {
                self.runtime = Some(Arc::new(*value));
                return Err(Fault::Profile);
            }
        };
        let state = self.removal.take().expect("validated removal obligation");
        Ok(SettledRemoval {
            lease,
            reservation: state.reservation,
            evidence: RemovalEvidence {
                intent: state.intent,
                scope: self.scope.clone(),
                canonical: self.association_scope,
                attempt: state.attempt,
            },
        })
    }
}
pub(super) fn removal_fault(fault: crate::removal::Fault) -> Fault {
    match fault {
        crate::removal::Fault::Cancelled => Fault::Cancelled,
        crate::removal::Fault::Scope => Fault::ScopeChanged,
        crate::removal::Fault::Grant => Fault::PersistenceUnavailable,
        crate::removal::Fault::NotSignedOut => Fault::NotConnected,
        crate::removal::Fault::State | crate::removal::Fault::Correlation => Fault::Protocol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_deadline_and_exact_action_cannot_be_replaced_or_renewed() {
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(1));
        let action = RemovalAdmission::from_native(cancel.clone(), generation.clone(), 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let setup = AccountPreparation::new(&action, deadline).unwrap();
        assert!(setup.joins(&action, deadline).is_ok());
        assert!(matches!(
            setup.joins(&action, deadline + RPC),
            Err(Fault::ScopeChanged)
        ));
        let other =
            RemovalAdmission::from_native(Arc::new(AtomicBool::new(false)), generation.clone(), 1)
                .unwrap();
        assert!(matches!(
            setup.joins(&other, deadline),
            Err(Fault::ScopeChanged)
        ));
        generation.store(2, Ordering::Release);
        assert!(matches!(setup.current(), Err(Fault::ScopeChanged)));
        cancel.store(true, Ordering::Release);
        assert!(matches!(setup.current(), Err(Fault::Cancelled)));
    }
    #[test]
    fn expired_setup_is_rejected_before_account_mutation() {
        let action = RemovalAdmission::from_native(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(1)),
            1,
        )
        .unwrap();
        assert!(matches!(
            AccountPreparation::new(&action, Instant::now() - Duration::from_secs(1)),
            Err(Fault::Timeout)
        ));
    }
    #[test]
    fn exact_retained_custody_can_move_between_native_owners_without_cloning() {
        fn require_send<T: Send>() {}
        require_send::<RemovalFailure>();
        require_send::<SettledRemoval>();
    }
}
