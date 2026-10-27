//! Observational quota RPCs share the exact owner's wire allocator and custody,
//! but never become authentication, turn admission, pricing or model deadlines.
use super::*;
pub use crate::quota_owner::QuotaView;
use crate::{quota::ReadTicket, quota_owner::QuotaOwner};
use zephium_provider_accounts::quota::QuotaFault;

pub(super) struct QuotaPending {
    pub(super) epoch: u64,
    /// None is an expired known-ID tombstone. The actual eventual reply is
    /// discarded once, never reassociated with a replacement account/read.
    pub(super) ticket: Option<ReadTicket>,
    pub(super) action: Option<crate::demand_budget::DemandBudget>,
}
impl Connection {
    pub fn quota(&self, scope: &Scope) -> Result<QuotaView, Fault> {
        self.current(scope)?;
        Ok(self.quota_owner.view())
    }
    /// Existing authenticated owner only; cannot discover/start a runtime,
    /// authenticate, redeem credits or bypass the connection-wide read budget.
    pub fn request_quota_refresh(
        &mut self,
        scope: &Scope,
        cancel: &AtomicBool,
    ) -> Result<(), Fault> {
        self.current(scope)?;
        if cancel.load(Ordering::Acquire) {
            return Err(Fault::Cancelled);
        }
        if self.removal.is_some()
            || self.removal_setup.is_some()
            || self.state() != AccountState::ChatGpt
        {
            self.publish_quota();
            return Ok(());
        }
        self.quota_owner.manual();
        self.schedule_quota(cancel)?;
        self.publish_quota();
        Ok(())
    }
    pub(super) fn new_quota_owner(scope: &Scope) -> QuotaOwner {
        QuotaOwner::new(scope.clone(), scope.native_generation())
    }
    pub(super) fn publish_quota(&mut self) {
        if let Some(view) = self.quota_owner.take_changed() {
            self.ready_events.push(self.event(None, Event::Quota(view)));
        }
    }
    pub(super) fn schedule_quota(&mut self, cancel: &AtomicBool) -> Result<(), Fault> {
        if self.removal.is_some()
            || self.removal_setup.is_some()
            || self.state() != AccountState::ChatGpt
            || self.session.is_none()
        {
            return Ok(());
        }
        let now = Instant::now();
        let followup = self.catalog_demand.clone();
        let scope = self.scope.clone();
        let wake_cancel = self.wake_cancel.as_ref();
        let wake_deadline = self.wake_deadline;
        let admitted = self.quota_owner.begin_bounded(
            &mut self.quota_demand,
            || {
                if let Some(action) = followup {
                    Ok(Some(action))
                } else {
                    crate::demand_budget::DemandBudget::from_wake(
                        scope,
                        wake_cancel,
                        wake_deadline,
                        now,
                    )
                }
            },
            now,
            wall_ms(),
        );
        let (epoch, ticket, action) = match admitted {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(()),
            Err(crate::demand_budget::Fault::Scope) => return self.fail(Fault::Protocol),
            Err(_) => return Ok(()),
        };
        let id = self.id()?;
        let deadline = (now + RPC).min(self.deadline);
        let deadline = self
            .wake_deadline
            .map_or(deadline, |limit| deadline.min(limit));
        let deadline = action
            .as_ref()
            .map_or(deadline, |action| action.cap(deadline));
        let fence = action
            .as_ref()
            .map(|action| windows::WriteFence::login(action.cancel()));
        self.pending.insert(
            id,
            Pending {
                kind: PendingKind::Quota(QuotaPending {
                    epoch,
                    ticket: Some(ticket),
                    action,
                }),
                deadline,
            },
        );
        // Only this fixed observational sender can consume a same-Session
        // pre-write rejection receipt without poisoning model/account custody.
        let result = self
            .session
            .as_mut()
            .ok_or(Fault::Closed)?
            .send_quota_read(id, deadline, cancel, fence)
            .map_err(transport);
        match result {
            Ok(windows::QuotaWrite::Written) => Ok(()),
            Ok(windows::QuotaWrite::Rejected) => {
                let Some(Pending {
                    kind: PendingKind::Quota(mut pending),
                    ..
                }) = self.pending.remove(&id)
                else {
                    return self.fail(Fault::Protocol);
                };
                if let Some(ticket) = pending.ticket.take() {
                    self.quota_owner.abandon(pending.epoch, ticket, wall_ms());
                }
                self.quota_owner.abandon_manual(wall_ms());
                Ok(())
            }
            Err(fault) => self.fail(fault),
        }
    }
    pub(super) fn quota_tick(&mut self) {
        let now = Instant::now();
        let at = wall_ms();
        for pending in self.pending.values_mut() {
            if let PendingKind::Quota(quota) = &mut pending.kind {
                if quota
                    .action
                    .as_ref()
                    .is_some_and(|action| action.check(&self.scope, now).is_err())
                {
                    if let Some(ticket) = quota.ticket.take() {
                        self.quota_owner.abandon(quota.epoch, ticket, at);
                    }
                    // Exact owned tombstone now eligible for the separately
                    // reviewed inert close drain; no new RPC or ID reuse.
                    pending.deadline = pending.deadline.min(now);
                } else if now >= pending.deadline {
                    if let Some(ticket) = quota.ticket.take() {
                        self.quota_owner
                            .failed(quota.epoch, ticket, QuotaFault::TimedOut, at);
                    }
                }
            }
        }
        self.quota_owner.mark_stale(at);
    }
    pub(super) fn quota_reply(
        &mut self,
        pending: QuotaPending,
        deadline: Instant,
        result: Result<Value, Value>,
    ) {
        let Some(ticket) = pending.ticket else {
            return;
        };
        if pending
            .action
            .as_ref()
            .is_some_and(|action| action.check(&self.scope, Instant::now()).is_err())
        {
            self.quota_owner.abandon(pending.epoch, ticket, wall_ms());
            return;
        }
        if Instant::now() >= deadline {
            self.quota_owner
                .failed(pending.epoch, ticket, QuotaFault::TimedOut, wall_ms());
            return;
        }
        match result {
            Ok(value) => self
                .quota_owner
                .complete(pending.epoch, ticket, &value, wall_ms()),
            Err(_) => self.quota_owner.failed(
                pending.epoch,
                ticket,
                QuotaFault::ProviderRefused,
                wall_ms(),
            ),
        }
    }
    pub(super) fn quota_notification(&mut self, params: &Value) {
        // The exact known notification is observational. Malformed percentage/
        // metadata updates the quota fault only, never auth/turn success/failure.
        self.quota_owner.updated(params, wall_ms());
        self.publish_quota();
    }
}
pub(super) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .unwrap_or(u64::MAX)
}
