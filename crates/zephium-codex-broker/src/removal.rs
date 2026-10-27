//! Protocol evidence for explicit local managed-account removal. This state is
//! not filesystem deletion authority or proof of remote OAuth revocation.
use crate::conversation::Scope;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use zephium_provider_accounts::codex::AccountState;

/// Native action lifetime. No Clone, deserialization, path, or reset operation.
pub struct RemovalAdmission {
    cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    expected_generation: u64,
}
impl RemovalAdmission {
    /// Only the native explicit-removal coordinator supplies these owned fences.
    /// This does not itself authorize logout, grant retirement, or data deletion.
    pub fn from_native(
        cancel: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
        expected_generation: u64,
    ) -> Option<Self> {
        (expected_generation != 0).then_some(Self {
            cancel,
            generation,
            expected_generation,
        })
    }
    #[cfg(all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    ))]
    pub(crate) fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
    #[cfg(all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    ))]
    pub(crate) fn writer_fence(&self) -> crate::windows::WriteFence {
        crate::windows::WriteFence::text(
            self.cancel.clone(),
            self.generation.clone(),
            self.expected_generation,
        )
    }
    pub(crate) fn current(&self) -> Result<(), Fault> {
        if self.cancel.load(Ordering::Acquire) {
            Err(Fault::Cancelled)
        } else if self.generation.load(Ordering::Acquire) != self.expected_generation {
            Err(Fault::Scope)
        } else {
            Ok(())
        }
    }
}

/// Historical observations only; never a token- or file-removal capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialObservation {
    Unconfirmed,
    LogoutAcknowledged,
    SignedOutObservedAfterLogout,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Scope,
    Cancelled,
    State,
    Grant,
    Correlation,
    NotSignedOut,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Request {
    Logout,
    Read,
}
#[derive(Clone, Copy)]
enum Phase {
    RetirementRequired,
    LogoutRequired,
    LogoutPending(u64),
    AwaitingLogoutNotification { after: u64 },
    ReadRequired { after: u64 },
    ReadPending(u64),
    Observed,
    Failed,
}

/// Move-only protocol obligation. The native owner supplies only actual
/// acknowledged retirement, handed-off IDs and already-validated reducer state.
/// Physical child/pipe settlement and same-lease custody must be joined later.
pub(crate) struct RemovalIntent {
    scope: Scope,
    incarnation: [u8; 16],
    grant_before: u64,
    grant_after: Option<u64>,
    phase: Phase,
    observation: CredentialObservation,
    admission: RemovalAdmission,
}
impl RemovalIntent {
    pub(crate) fn new(
        scope: Scope,
        incarnation: [u8; 16],
        grant_before: u64,
        admission: RemovalAdmission,
    ) -> Result<Self, Fault> {
        admission.current()?;
        if incarnation == [0; 16] || grant_before == 0 || grant_before == u64::MAX {
            return Err(Fault::Grant);
        }
        Ok(Self {
            scope,
            incarnation,
            grant_before,
            grant_after: None,
            phase: Phase::RetirementRequired,
            observation: CredentialObservation::Unconfirmed,
            admission,
        })
    }
    fn scope(&self, scope: &Scope) -> Result<(), Fault> {
        self.admission.current()?;
        if scope != &self.scope {
            Err(Fault::Scope)
        } else {
            Ok(())
        }
    }
    fn fail<T>(&mut self, fault: Fault) -> Result<T, Fault> {
        self.phase = Phase::Failed;
        Err(fault)
    }
    /// Called only with the canonical Store's exact durable retirement receipt.
    /// A lost acknowledgement cannot be replaced by guessing `old + 1`.
    pub(crate) fn retired(
        &mut self,
        scope: &Scope,
        incarnation: [u8; 16],
        grant: u64,
    ) -> Result<(), Fault> {
        self.scope(scope)?;
        if !matches!(self.phase, Phase::RetirementRequired)
            || incarnation != self.incarnation
            || self.grant_before.checked_add(1) != Some(grant)
        {
            return self.fail(Fault::Grant);
        }
        self.grant_after = Some(grant);
        self.phase = Phase::LogoutRequired;
        Ok(())
    }
    /// Correlates the one actual original request; does not certify a write.
    /// A send error still retires this transaction without a completion receipt.
    pub(crate) fn outgoing(
        &mut self,
        scope: &Scope,
        id: u64,
        request: Request,
        current_grant: u64,
    ) -> Result<(), Fault> {
        self.scope(scope)?;
        if id == 0 || self.grant_after != Some(current_grant) {
            return self.fail(Fault::Correlation);
        }
        self.phase = match (self.phase, request) {
            (Phase::LogoutRequired, Request::Logout) => Phase::LogoutPending(id),
            (Phase::ReadRequired { after }, Request::Read) if id > after => Phase::ReadPending(id),
            _ => return self.fail(Fault::State),
        };
        Ok(())
    }
    /// `state` must come from ManagedAccount after this correlated reply was
    /// validated. An account notification never calls this method.
    pub(crate) fn replied(
        &mut self,
        scope: &Scope,
        id: u64,
        state: AccountState,
    ) -> Result<(), Fault> {
        self.scope(scope)?;
        match self.phase {
            Phase::LogoutPending(expected) if id == expected && state == AccountState::Checking => {
                self.observation = CredentialObservation::LogoutAcknowledged;
                self.phase = Phase::AwaitingLogoutNotification { after: id };
                Ok(())
            }
            Phase::ReadPending(expected) if id == expected => {
                if state != AccountState::SignedOut {
                    return self.fail(Fault::NotSignedOut);
                }
                self.observation = CredentialObservation::SignedOutObservedAfterLogout;
                self.phase = Phase::Observed;
                Ok(())
            }
            _ => self.fail(Fault::Correlation),
        }
    }
    /// Only a validated account/updated notification enters here. Both qualified
    /// original versions send logout ACK before that notification. The queued
    /// follow-up read stays unsent until this barrier, under the same deadline.
    pub(crate) fn account_invalidated(&mut self) -> Result<bool, Fault> {
        self.admission.current()?;
        match self.phase {
            Phase::AwaitingLogoutNotification { after } => {
                self.phase = Phase::ReadRequired { after };
                Ok(true)
            }
            Phase::ReadRequired { .. }
            | Phase::ReadPending(_)
            | Phase::Observed
            | Phase::Failed => self.fail(Fault::State),
            _ => Ok(false),
        }
    }
    #[cfg(test)]
    pub(crate) fn waiting_for_logout_notification(&self) -> bool {
        matches!(self.phase, Phase::AwaitingLogoutNotification { .. })
    }
    /// Recheck both before and after physical finish, and before the remover's
    /// protected tombstone. This is still not a physical-settlement receipt.
    pub(crate) fn observed(
        &self,
        scope: &Scope,
        incarnation: [u8; 16],
        current_grant: u64,
    ) -> Result<(), Fault> {
        self.scope(scope)?;
        if incarnation != self.incarnation || self.grant_after != Some(current_grant) {
            return Err(Fault::Grant);
        }
        if !matches!(self.phase, Phase::Observed) {
            return Err(Fault::State);
        }
        Ok(())
    }
    #[cfg(all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    ))]
    pub(crate) fn current(&self) -> Result<(), Fault> {
        self.admission.current()
    }
    #[cfg(all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    ))]
    pub(crate) fn writer_fence(&self) -> crate::windows::WriteFence {
        self.admission.writer_fence()
    }
    pub(crate) fn observation(&self) -> CredentialObservation {
        self.observation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope::new("synthetic-native-profile".into(), [3; 16], 4).unwrap()
    }
    fn intent() -> (RemovalIntent, Arc<AtomicBool>, Arc<AtomicU64>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(7));
        let intent = RemovalIntent::new(
            scope(),
            [2; 16],
            8,
            RemovalAdmission::from_native(cancel.clone(), generation.clone(), 7).unwrap(),
        )
        .unwrap();
        (intent, cancel, generation)
    }
    fn acknowledge(intent: &mut RemovalIntent) {
        intent.retired(&scope(), [2; 16], 9).unwrap();
        intent.outgoing(&scope(), 11, Request::Logout, 9).unwrap();
        intent
            .replied(&scope(), 11, AccountState::Checking)
            .unwrap();
        assert!(intent.waiting_for_logout_notification());
        assert!(intent.account_invalidated().unwrap());
    }
    fn observed(intent: &mut RemovalIntent) {
        acknowledge(intent);
        intent.outgoing(&scope(), 12, Request::Read, 9).unwrap();
        intent
            .replied(&scope(), 12, AccountState::SignedOut)
            .unwrap();
    }
    #[test]
    fn exact_logout_ack_then_fresh_read_are_both_mandatory() {
        let (mut intent, _, _) = intent();
        assert!(intent.observed(&scope(), [2; 16], 9).is_err());
        acknowledge(&mut intent);
        assert_eq!(
            intent.observation(),
            CredentialObservation::LogoutAcknowledged
        );
        assert!(intent.observed(&scope(), [2; 16], 9).is_err());
        intent.outgoing(&scope(), 12, Request::Read, 9).unwrap();
        intent
            .replied(&scope(), 12, AccountState::SignedOut)
            .unwrap();
        assert!(intent.observed(&scope(), [2; 16], 9).is_ok());
        assert_eq!(
            intent.observation(),
            CredentialObservation::SignedOutObservedAfterLogout
        );
    }
    #[test]
    fn read_only_signedout_and_forged_or_reordered_ids_never_complete() {
        for case in 0..6 {
            let (mut intent, _, _) = intent();
            intent.retired(&scope(), [2; 16], 9).unwrap();
            let result = match case {
                0 => intent.outgoing(&scope(), 11, Request::Read, 9),
                1 => intent.replied(&scope(), 11, AccountState::SignedOut),
                _ => {
                    intent.outgoing(&scope(), 11, Request::Logout, 9).unwrap();
                    match case {
                        2 => intent.replied(&scope(), 12, AccountState::Checking),
                        3 => intent.replied(&scope(), 11, AccountState::SignedOut),
                        _ => {
                            intent
                                .replied(&scope(), 11, AccountState::Checking)
                                .unwrap();
                            assert!(intent.account_invalidated().unwrap());
                            if case == 4 {
                                intent.outgoing(&scope(), 11, Request::Read, 9)
                            } else {
                                intent.outgoing(&scope(), 12, Request::Read, 9).unwrap();
                                intent.replied(&scope(), 13, AccountState::SignedOut)
                            }
                        }
                    }
                }
            };
            assert!(result.is_err());
            assert!(intent.observed(&scope(), [2; 16], 9).is_err());
        }
    }
    #[test]
    fn grant_retirement_must_be_exact_and_acknowledged_before_logout() {
        for case in 0..5 {
            let (mut intent, _, _) = intent();
            let result = match case {
                0 => intent.outgoing(&scope(), 11, Request::Logout, 9),
                1 => intent.retired(&scope(), [4; 16], 9),
                2 => intent.retired(&scope(), [2; 16], 8),
                3 => intent.retired(&scope(), [2; 16], 10),
                _ => {
                    intent.retired(&scope(), [2; 16], 9).unwrap();
                    intent.outgoing(&scope(), 11, Request::Logout, 8)
                }
            };
            assert!(result.is_err());
            assert!(intent.observed(&scope(), [2; 16], 9).is_err());
        }
    }
    #[test]
    fn scope_cancel_and_late_account_change_retire_observed_evidence() {
        for case in 0..4 {
            let (mut intent, cancel, generation) = intent();
            observed(&mut intent);
            assert!(intent.observed(&scope(), [2; 16], 9).is_ok());
            match case {
                0 => cancel.store(true, Ordering::Release),
                1 => {
                    generation.fetch_add(1, Ordering::AcqRel);
                }
                2 => {
                    assert!(intent.account_invalidated().is_err());
                }
                _ => {
                    assert_eq!(intent.observed(&scope(), [2; 16], 10), Err(Fault::Grant));
                    assert_eq!(intent.observed(&scope(), [9; 16], 9), Err(Fault::Grant));
                    let other = Scope::new("other-profile".into(), [4; 16], 5).unwrap();
                    assert_eq!(intent.observed(&other, [2; 16], 9), Err(Fault::Scope));
                    continue;
                }
            }
            assert!(intent.observed(&scope(), [2; 16], 9).is_err());
            // Historical observation is retained, never upgraded to deletion or
            // remote revocation after its current admission has been retired.
            assert_eq!(
                intent.observation(),
                CredentialObservation::SignedOutObservedAfterLogout
            );
        }
    }
    #[test]
    fn expected_logout_notification_is_before_fresh_read_and_later_changes_refuse() {
        let (mut first, _, _) = intent();
        first.retired(&scope(), [2; 16], 9).unwrap();
        first.outgoing(&scope(), 11, Request::Logout, 9).unwrap();
        // A prior account notification does not fulfill the post-ACK barrier.
        assert!(!first.account_invalidated().unwrap());
        first.replied(&scope(), 11, AccountState::Checking).unwrap();
        assert!(first.waiting_for_logout_notification());
        assert!(first.outgoing(&scope(), 12, Request::Read, 9).is_err());
        assert!(first.observed(&scope(), [2; 16], 9).is_err());

        let (mut second, _, _) = intent();
        acknowledge(&mut second);
        second.outgoing(&scope(), 12, Request::Read, 9).unwrap();
        // In-flight invalidation cannot be repaired by the now-stale reply.
        assert!(second.account_invalidated().is_err());
        assert!(second
            .replied(&scope(), 12, AccountState::SignedOut)
            .is_err());
        assert!(second.observed(&scope(), [2; 16], 9).is_err());
    }
    #[test]
    fn read_still_signed_in_or_bad_grant_has_no_removal_observation() {
        let (mut intent, _, _) = intent();
        acknowledge(&mut intent);
        intent.outgoing(&scope(), 12, Request::Read, 9).unwrap();
        assert_eq!(
            intent.replied(&scope(), 12, AccountState::ChatGpt),
            Err(Fault::NotSignedOut)
        );
        assert_eq!(
            intent.observation(),
            CredentialObservation::LogoutAcknowledged
        );
        assert!(intent.observed(&scope(), [2; 16], 9).is_err());
        assert!(RemovalAdmission::from_native(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            0
        )
        .is_none());
    }
    fn account() -> crate::managed_account::ManagedAccount {
        use crate::managed_account::{InitializedAccountReceipt, ManagedAccount};
        ManagedAccount::from_initialized(
            InitializedAccountReceipt::from_native_handshake(
                scope(),
                1,
                serde_json::json!({"userAgent":"synthetic-original"}),
            )
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn deferred_managed_logout_creates_only_one_fresh_read_after_notification() {
        use crate::managed_account::{Effect, LogoutAction, Notification};
        let mut account = account();
        // Already signed out still requires the original logout operation.
        account.read(&scope()).unwrap();
        let initial = account.take_request(&scope(), 2).unwrap();
        assert_eq!(initial.into_json()["method"], "account/read");
        account
            .reply(
                &scope(),
                2,
                Ok(serde_json::json!({"account":null,"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert_eq!(account.state(&scope()).unwrap(), AccountState::SignedOut);
        let (mut intent, _, _) = intent();
        intent.retired(&scope(), [2; 16], 9).unwrap();
        let event = account
            .logout_for_removal(LogoutAction::from_native_action(scope()))
            .unwrap();
        assert!(matches!(event.into_effect(), Effect::RequestQueued));
        let logout = account.take_request(&scope(), 11).unwrap();
        assert_eq!(
            logout.into_json(),
            serde_json::json!({"id":11,"method":"account/logout"})
        );
        intent.outgoing(&scope(), 11, Request::Logout, 9).unwrap();
        let event = account
            .reply(&scope(), 11, Ok(serde_json::json!({})))
            .unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::Checking)
        ));
        intent
            .replied(&scope(), 11, account.state(&scope()).unwrap())
            .unwrap();
        assert!(intent.waiting_for_logout_notification());
        // There is not even a logical Read to be invalidated before its handoff.
        assert!(account.take_request(&scope(), 12).is_err());
        let event = account
            .notification(
                &scope(),
                Notification::AccountUpdated,
                serde_json::json!({"authMode":null}),
            )
            .unwrap();
        assert!(event.invalidates_admission());
        assert!(intent.account_invalidated().unwrap());
        assert!(matches!(
            account.read(&scope()).unwrap().into_effect(),
            Effect::RequestQueued
        ));
        let read = account.take_request(&scope(), 12).unwrap();
        assert_eq!(
            read.into_json(),
            serde_json::json!({"id":12,"method":"account/read","params":{"refreshToken":false}})
        );
        intent.outgoing(&scope(), 12, Request::Read, 9).unwrap();
        let event = account
            .reply(
                &scope(),
                12,
                Ok(serde_json::json!({"account":null,"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::SignedOut)
        ));
        intent
            .replied(&scope(), 12, account.state(&scope()).unwrap())
            .unwrap();
        assert!(intent.observed(&scope(), [2; 16], 9).is_ok());
        assert!(account.take_request(&scope(), 13).is_err());
    }
    #[test]
    fn deferred_confirmation_preserves_inflight_invalidation_and_does_not_retry_removal() {
        use crate::managed_account::{Effect, LogoutAction, Notification};
        let mut account = account();
        let (mut intent, _, _) = intent();
        intent.retired(&scope(), [2; 16], 9).unwrap();
        account
            .logout_for_removal(LogoutAction::from_native_action(scope()))
            .unwrap();
        account.take_request(&scope(), 11).unwrap();
        intent.outgoing(&scope(), 11, Request::Logout, 9).unwrap();
        account
            .reply(&scope(), 11, Ok(serde_json::json!({})))
            .unwrap();
        intent
            .replied(&scope(), 11, account.state(&scope()).unwrap())
            .unwrap();
        account
            .notification(
                &scope(),
                Notification::AccountUpdated,
                serde_json::json!({"authMode":null}),
            )
            .unwrap();
        assert!(intent.account_invalidated().unwrap());
        account.read(&scope()).unwrap();
        account.take_request(&scope(), 12).unwrap();
        intent.outgoing(&scope(), 12, Request::Read, 9).unwrap();
        account
            .notification(
                &scope(),
                Notification::AccountUpdated,
                serde_json::json!({"authMode":"chatgpt"}),
            )
            .unwrap();
        assert!(intent.account_invalidated().is_err());
        // Existing ordinary reducer freshness behavior is unchanged. The
        // removal owner closes instead of handing off this retry or a receipt.
        let event = account
            .reply(
                &scope(),
                12,
                Ok(serde_json::json!({"account":null,"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert!(matches!(event.into_effect(), Effect::RequestQueued));
        assert!(intent
            .replied(&scope(), 12, account.state(&scope()).unwrap())
            .is_err());
        account.close(&scope()).unwrap();
        assert!(account.take_request(&scope(), 13).is_err());
    }
    #[test]
    fn ordinary_logout_keeps_automatic_read_and_requires_typed_confirmation() {
        use crate::managed_account::{Effect, LogoutAction};
        let mut account = account();
        account
            .logout(LogoutAction::from_native_action(scope()))
            .unwrap();
        let logout = account.take_request(&scope(), 11).unwrap();
        assert_eq!(
            logout.into_json(),
            serde_json::json!({"id":11,"method":"account/logout"})
        );
        let event = account
            .reply(&scope(), 11, Ok(serde_json::json!({})))
            .unwrap();
        assert!(matches!(event.into_effect(), Effect::RequestQueued));
        assert_eq!(
            account.take_request(&scope(), 12).unwrap().into_json(),
            serde_json::json!({"id":12,"method":"account/read","params":{"refreshToken":false}})
        );
        let mut reducer = zephium_provider_accounts::codex::CodexAccountSession::new();
        assert!(reducer.logout_deferred_read(false).is_err());
    }
}

mod retry;
#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
pub use retry::{RemovalRetryAdmission, RemovalRetryFault};
