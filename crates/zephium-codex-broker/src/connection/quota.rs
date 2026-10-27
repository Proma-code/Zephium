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
        let Some((epoch, ticket)) = self.quota_owner.begin(now, wall_ms()) else {
            return Ok(());
        };
        let id = self.id()?;
        let deadline = (now + RPC).min(self.deadline);
        self.pending.insert(
            id,
            Pending {
                kind: PendingKind::Quota(QuotaPending {
                    epoch,
                    ticket: Some(ticket),
                }),
                deadline,
            },
        );
        // A broken pipe/process is still an owner custody failure. Only the
        // observed result/parse/deadline is nonfatal, not a failed transport.
        self.send(serde_json::json!({"id":id,"method":"account/rateLimits/read","params":crate::quota::AccountQuota::read_params()}), deadline, cancel)
    }
    pub(super) fn quota_tick(&mut self) {
        let now = Instant::now();
        let at = wall_ms();
        for pending in self.pending.values_mut() {
            if let PendingKind::Quota(quota) = &mut pending.kind {
                if now >= pending.deadline {
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
fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .unwrap_or(u64::MAX)
}
