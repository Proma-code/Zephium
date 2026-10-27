//! Healthy idle physical retirement and existing-owner wake. These are not
//! logout, account/grant retirement, cleanup authority, or a retry of a turn.
use super::*;

pub(super) struct Identity {
    profile: ProfileId,
    scope: Scope,
    canonical: durable::Scope,
    // Data only: fixed-node identities/config hashes, never retained handles.
    record: Box<crate::profile::Record>,
}
/// Produced only after a graceful original exit and ALL Connection-owned image,
/// runtime and lease pins have been dropped. Contains correlation data only.
pub struct DormantReceipt {
    identity: Identity,
    quota_unknown: bool,
}
impl DormantReceipt {
    /// Whether expired observation IDs needed the inert close drain. False does
    /// not attest quota freshness: EVERY dormant transition retires cached quota,
    /// including an unissued scheduled refresh, and carries no quota reading.
    pub fn quota_unknown(&self) -> bool {
        self.quota_unknown
    }
    pub fn profile(&self) -> ProfileId {
        self.identity.profile
    }
    pub fn scope(&self) -> &Scope {
        &self.identity.scope
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DormantFailureKind {
    NotReady,
    Failed,
}
/// Retains the exact owner, including uncertain/failed physical custody.
pub struct DormantFailure {
    kind: DormantFailureKind,
    fault: Fault,
    owner: Box<Connection>,
}
impl DormantFailure {
    /// Only transient Busy preserves eligibility of this same unconsumed owner.
    /// NotReady alone is not retry authority and never renews an action budget.
    pub fn retryable_busy(&self) -> bool {
        retryable_busy(self.kind, self.fault)
    }
    pub fn into_parts(self) -> (DormantFailureKind, Fault, Box<Connection>) {
        (self.kind, self.fault, self.owner)
    }
}
fn retryable_busy(kind: DormantFailureKind, fault: Fault) -> bool {
    kind == DormantFailureKind::NotReady && fault == Fault::Busy
}
/// Historical correlation after a failed wake, deliberately NOT DormantReceipt.
/// Native UI must expose failure; only a new explicit demand may consume this.
pub struct DormantWakeFailure {
    fault: Fault,
    identity: Identity,
}
impl DormantWakeFailure {
    pub fn fault(&self) -> Fault {
        self.fault
    }
    pub fn profile(&self) -> ProfileId {
        self.identity.profile
    }
    pub fn scope(&self) -> &Scope {
        &self.identity.scope
    }
}
impl Connection {
    /// Observational readiness; the consuming operation repeats this predicate
    /// after the native queue/turn reservation gate has been sealed separately.
    pub fn ready_to_dormant(&self, scope: &Scope) -> bool {
        crate::dormancy::Quiescence {
            current: self.current(scope).is_ok(),
            persistent: self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.persistent_identity())
                .is_some(),
            account_stable: matches!(
                self.state(),
                AccountState::ChatGpt | AccountState::SignedOut
            ),
            account_idle: self.account.removal_ready(scope).is_ok(),
            pending: self.pending.len(),
            expired_quota: self.expired_quota_for_dormant().len(),
            quota_idle: self.quota_owner.quiescent(),
            active: self.active.is_some(),
            prompt: self.prompt.is_some(),
            catalog: self.catalog_builder.is_some(),
            login: self.login_deadline.is_some() || self.account_deadline.is_some(),
            closing: self.closing_deadline.is_some(),
            removal: self.removal.is_some() || self.removal_setup.is_some(),
            browser: self.browser.is_some(),
            wake: self.wake_deadline.is_some()
                || self.wake_origin.is_some()
                || self.account_demand.is_some()
                || self.catalog_demand.is_some()
                || self.quota_demand.is_some(),
            queued_facts: self
                .ready_events
                .len()
                .saturating_add(self.failure_events.len()),
            threads_closed: self
                .conversations
                .values()
                .all(|thread| thread.ready_to_dormant(scope)),
        }
        .ready()
    }
    pub fn try_dormant(
        mut self,
        scope: &Scope,
        absolute_deadline: Instant,
    ) -> Result<DormantReceipt, DormantFailure> {
        let deadline = absolute_deadline.min(Instant::now() + CLOSE);
        if let Err(fault) = self.current(scope) {
            return Err(self.dormant_failure(DormantFailureKind::NotReady, fault));
        }
        if Instant::now() >= deadline || !self.ready_to_dormant(scope) {
            let fault = if Instant::now() >= deadline {
                Fault::Timeout
            } else {
                Fault::Busy
            };
            return Err(self.dormant_failure(DormantFailureKind::NotReady, fault));
        }
        let Some(record) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.persistent_record())
            .cloned()
        else {
            return Err(self.dormant_failure(DormantFailureKind::NotReady, Fault::Profile));
        };
        let profile = record.profile;
        let incarnation = record.incarnation;
        let canonical = self.association_scope;
        if canonical.incarnation != incarnation
            || self.associations.call(durable::Call::InspectOwner)
                != durable::Reply::Owner(Some(canonical))
        {
            return Err(
                self.dormant_failure(DormantFailureKind::NotReady, Fault::PersistenceUnavailable)
            );
        }
        if Instant::now() >= deadline {
            return Err(self.dormant_failure(DormantFailureKind::NotReady, Fault::Timeout));
        }
        let expired = self.expired_quota_for_dormant();
        let quota_unknown = !expired.is_empty();
        self.quota_owner.retire_for_dormant();
        // Session::finish never promotes forced/failed cleanup into a graceful
        // result. It requires EOF, no unclassified unread frames, Job0, joined
        // writer and CLEAN. Only the explicit expired-quota drain classifies tails.
        let session = self.session.take().expect("current owner checked");
        let result = if expired.is_empty() {
            session.finish(deadline)
        } else {
            session.finish_dormant_quota(deadline, &self.scope, expired)
        }
        .map_err(transport)
        .and_then(|exit| {
            if exit.code == 0 {
                Ok(())
            } else {
                Err(Fault::Transport)
            }
        });
        if let Err(fault) = result {
            let _ = self.fail::<()>(fault);
            return Err(self.dormant_failure(DormantFailureKind::Failed, fault));
        }
        let clean = self.runtime.as_ref().is_some_and(|runtime| {
            Arc::strong_count(runtime) == 1 && runtime.verify_dormant_clean().is_ok()
        });
        if !clean
            || self.associations.call(durable::Call::InspectOwner)
                != durable::Reply::Owner(Some(canonical))
        {
            return Err(
                self.dormant_failure(DormantFailureKind::Failed, Fault::PersistenceUnavailable)
            );
        }
        if Instant::now() >= deadline {
            return Err(self.dormant_failure(DormantFailureKind::Failed, Fault::Timeout));
        }
        let identity = Identity {
            profile,
            scope: self.scope.clone(),
            canonical,
            record: Box::new(record),
        };
        // This drop includes BrowserCapability's Arc<PinnedImage>, not merely
        // Session. No receipt is observable while an owned package pin remains.
        drop(self);
        Ok(DormantReceipt {
            identity,
            quota_unknown,
        })
    }
    fn expired_quota_for_dormant(&self) -> Vec<(u64, u64)> {
        let now = Instant::now();
        self.pending
            .iter()
            .filter_map(|(id, pending)| match &pending.kind {
                PendingKind::Quota(quota)
                    if quota.ticket.is_none() && pending.deadline <= now && quota.epoch != 0 =>
                {
                    Some((*id, quota.epoch))
                }
                _ => None,
            })
            .collect()
    }
    pub(super) fn complete_wake(&mut self, cancel: &AtomicBool) -> Result<(), Fault> {
        self.check_demand_actions()?;
        let Some(deadline) = self.wake_deadline else {
            return Ok(());
        };
        check(deadline, cancel).or_else(|fault| self.fail(fault))?;
        if self.associations.call(durable::Call::InspectOwner)
            != durable::Reply::Owner(Some(self.association_scope))
        {
            return self.fail(Fault::PersistenceUnavailable);
        }
        check(deadline, cancel).or_else(|fault| self.fail(fault))?;
        self.wake_deadline = None;
        self.wake_origin = None;
        self.wake_cancel = None;
        Ok(())
    }
    fn dormant_failure(self, kind: DormantFailureKind, fault: Fault) -> DormantFailure {
        DormantFailure {
            kind,
            fault,
            owner: Box::new(self),
        }
    }
    pub fn open_dormant(
        receipt: DormantReceipt,
        epoch: u64,
        associations: Box<dyn Associations>,
        cached_release: Option<&str>,
        cancel: &Arc<AtomicBool>,
        absolute_deadline: Instant,
    ) -> Result<Self, DormantWakeFailure> {
        Self::wake_dormant(
            receipt.identity,
            epoch,
            associations,
            cached_release,
            cancel,
            absolute_deadline,
        )
    }
    /// A failed wake cannot recreate healthy Dormant. This distinct historical
    /// failure token is consumed only on a fresh explicit native wake demand.
    pub fn retry_dormant(
        failure: DormantWakeFailure,
        epoch: u64,
        associations: Box<dyn Associations>,
        cached_release: Option<&str>,
        cancel: &Arc<AtomicBool>,
        absolute_deadline: Instant,
    ) -> Result<Self, DormantWakeFailure> {
        Self::wake_dormant(
            failure.identity,
            epoch,
            associations,
            cached_release,
            cancel,
            absolute_deadline,
        )
    }
    /// Initial account/catalog polling can fail after open returned an owner.
    /// Only that exact, now physically closed wake can yield historical retry
    /// correlation. No ordinary owner or successful wake has this origin token.
    pub fn into_failed_dormant_wake(
        mut self,
        scope: &Scope,
        fault: Fault,
    ) -> Result<DormantWakeFailure, Box<Self>> {
        if self.browser.is_some() || self.removal.is_some() || self.removal_setup.is_some() {
            return Err(Box::new(self));
        }
        let closed = self.is_closed();
        let Some(identity) = crate::dormancy::take_failed_wake(
            &mut self.wake_origin,
            scope == &self.scope,
            closed,
            self.wake_deadline.is_some(),
        ) else {
            return Err(Box::new(self));
        };
        drop(self);
        Ok(DormantWakeFailure { fault, identity })
    }
    fn wake_dormant(
        identity: Identity,
        epoch: u64,
        associations: Box<dyn Associations>,
        cached_release: Option<&str>,
        cancel: &Arc<AtomicBool>,
        absolute_deadline: Instant,
    ) -> Result<Self, DormantWakeFailure> {
        let budget = PreparationBudget::bounded(Instant::now(), absolute_deadline);
        let result = Self::open_with_budget(
            identity.profile,
            epoch,
            associations,
            cached_release,
            cancel,
            NativePreparation {
                budget,
                removal: None,
                existing_owner: Some(identity.canonical),
                expected_record: Some(identity.record.clone()),
                wake_deadline: Some(budget.deadline()),
                wake_cancel: Some(cancel.clone()),
            },
        );
        match result {
            Ok(mut owner) => {
                owner.wake_origin = Some(identity);
                Ok(owner)
            }
            Err(fault) => Err(DormantWakeFailure { fault, identity }),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_not_ready_busy_keeps_same_owner_eligible() {
        for kind in [DormantFailureKind::NotReady, DormantFailureKind::Failed] {
            for fault in [
                Fault::Busy,
                Fault::ScopeChanged,
                Fault::PersistenceUnavailable,
                Fault::Profile,
                Fault::Timeout,
                Fault::Cancelled,
                Fault::Transport,
            ] {
                assert_eq!(
                    retryable_busy(kind, fault),
                    kind == DormantFailureKind::NotReady && fault == Fault::Busy
                );
            }
        }
    }
    #[test]
    fn dormant_correlation_and_failures_are_send_without_clone_or_image_authority() {
        fn send<T: Send>() {}
        send::<DormantReceipt>();
        send::<DormantFailure>();
        send::<DormantWakeFailure>();
    }
}

#[cfg(all(debug_assertions, feature = "native-fixtures"))]
#[path = "dormant_fixtures.rs"]
pub(crate) mod fixtures;
