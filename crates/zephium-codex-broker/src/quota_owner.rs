//! Connection-wide observation budget and projection. This does not create an
//! account, model request or transport; a native owner supplies exact RPC tickets.
use crate::{
    conversation::Scope,
    quota::{AccountQuota, ReadTicket},
};
use serde_json::Value;
use std::time::{Duration, Instant};
use zephium_provider_accounts::quota::{
    QuotaFault, QuotaProvenance, QuotaSource, QuotaState, QuotaUnavailable,
};
const INTERVAL: Duration = Duration::from_secs(30);
const MAX_CONNECTION_READS: u16 = 512;
const FRESH_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaView {
    pub state: QuotaState,
    pub profile_revision: u64,
    pub account_revision: u64,
    pub checking: bool,
    pub refresh_needed: bool,
    pub last_attempt_at_ms: Option<u64>,
}
/// Credential-free qualification admits only an exact current, empty projection.
/// This is not a quota read receipt and never changes production admission.
#[cfg(any(test, all(windows, debug_assertions, feature = "native-fixtures")))]
pub(crate) fn qualify_initial_quota(
    scope: &Scope,
    event_scope: &Scope,
    has_conversation: bool,
    state: zephium_provider_accounts::codex::AccountState,
    current: &QuotaView,
    observed: &QuotaView,
) -> bool {
    use zephium_provider_accounts::codex::AccountState;
    let unavailable = match state {
        AccountState::Checking => QuotaUnavailable::ObserverNotConnected,
        AccountState::SignedOut => QuotaUnavailable::SignedOut,
        _ => return false,
    };
    scope == event_scope
        && !has_conversation
        && observed == current
        && observed.profile_revision != 0
        && observed.account_revision != 0
        && !observed.checking
        && !observed.refresh_needed
        && observed.last_attempt_at_ms.is_none()
        && observed.state == QuotaState::Unavailable(unavailable)
}
pub(crate) struct QuotaOwner {
    scope: Scope,
    profile_revision: u64,
    epoch: u64,
    observer: Option<AccountQuota>,
    unavailable: QuotaUnavailable,
    reads: u16,
    last_started: Option<Instant>,
    last_attempt_at_ms: Option<u64>,
    checking: bool,
    scheduled: bool,
    dirty: bool,
}
impl QuotaOwner {
    pub(crate) fn new(scope: Scope, profile_revision: u64) -> Self {
        Self {
            scope,
            profile_revision,
            epoch: 1,
            observer: None,
            unavailable: QuotaUnavailable::ObserverNotConnected,
            reads: 0,
            last_started: None,
            last_attempt_at_ms: None,
            checking: false,
            scheduled: false,
            dirty: true,
        }
    }
    #[cfg(test)]
    fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Authentication invalidation clears old-account data without refunding
    /// the physical connection's quota cadence or finite observation allowance.
    pub(crate) fn invalidate(&mut self, signed_out: bool) {
        self.unavailable = if signed_out {
            QuotaUnavailable::SignedOut
        } else {
            QuotaUnavailable::ObserverNotConnected
        };
        if let Some(observer) = &mut self.observer {
            observer.retire(self.unavailable);
        }
        self.observer = None;
        self.epoch = if self.epoch == 0 {
            0
        } else {
            self.epoch.checked_add(1).unwrap_or(0)
        };
        self.checking = false;
        self.scheduled = false;
        self.last_attempt_at_ms = None;
        self.dirty = true;
    }
    pub(crate) fn ready(&mut self) {
        if self.observer.is_some() || self.epoch == 0 || self.profile_revision == 0 {
            return;
        }
        self.observer = AccountQuota::new(
            self.scope.clone(),
            QuotaProvenance {
                source: QuotaSource::CodexAppServer,
                profile_revision: self.profile_revision,
                account_revision: self.epoch,
            },
        )
        .ok();
        self.scheduled = self.observer.is_some();
        self.dirty = true;
    }
    pub(crate) fn signed_out(&mut self) {
        // A fresh signed-out read is independently authoritative even when no
        // account/updated notification preceded it. Retire old tickets/data.
        self.invalidate(true);
    }
    pub(crate) fn manual(&mut self) {
        if let Some(observer) = &mut self.observer {
            self.scheduled = true;
            let needed = observer.needs_refresh();
            let _ = observer.request_refresh(&self.scope, self.epoch);
            self.dirty |= needed != observer.needs_refresh();
        }
    }
    pub(crate) fn begin(&mut self, now: Instant, at_ms: u64) -> Option<(u64, ReadTicket)> {
        if !self.scheduled
            || self.reads >= MAX_CONNECTION_READS
            || self.last_started.is_some_and(|last| {
                now.checked_duration_since(last)
                    .is_none_or(|age| age < INTERVAL)
            })
        {
            return None;
        }
        let observer = self.observer.as_mut()?;
        if !observer.needs_refresh() {
            return None;
        }
        let ticket = observer.begin_read(&self.scope, self.epoch, now).ok()?;
        self.reads += 1;
        self.scheduled = false;
        self.last_started = Some(now);
        self.last_attempt_at_ms = Some(at_ms);
        self.checking = true;
        self.dirty = true;
        Some((self.epoch, ticket))
    }
    pub(crate) fn complete(&mut self, epoch: u64, ticket: ReadTicket, result: &Value, at_ms: u64) {
        if epoch != self.epoch {
            return;
        }
        if let Some(observer) = &mut self.observer {
            let _ = observer.complete_read(&self.scope, epoch, ticket, result, at_ms);
            if !observer.needs_refresh() {
                self.scheduled = false;
            }
            self.checking = false;
            self.dirty = true;
        }
    }
    pub(crate) fn failed(&mut self, epoch: u64, ticket: ReadTicket, fault: QuotaFault, at_ms: u64) {
        if epoch != self.epoch {
            return;
        }
        if let Some(observer) = &mut self.observer {
            let _ = observer.fail_read(&self.scope, epoch, ticket, fault, at_ms);
            self.checking = false;
            self.dirty = true;
        }
    }
    pub(crate) fn updated(&mut self, params: &Value, at_ms: u64) {
        if let Some(observer) = &mut self.observer {
            self.scheduled = true;
            let prior = observer.state().clone();
            let needed = observer.needs_refresh();
            let _ = observer.updated(&self.scope, self.epoch, params, at_ms);
            self.dirty |= prior != *observer.state() || needed != observer.needs_refresh();
        }
    }
    pub(crate) fn mark_stale(&mut self, at_ms: u64) {
        if let Some(observer) = &mut self.observer {
            let was_fresh = matches!(observer.state(), QuotaState::Available(_));
            observer.mark_stale(at_ms, FRESH_MS);
            if was_fresh && !matches!(observer.state(), QuotaState::Available(_)) {
                self.dirty = true;
            }
        }
    }
    pub(crate) fn view(&self) -> QuotaView {
        QuotaView {
            state: self
                .observer
                .as_ref()
                .map_or(QuotaState::Unavailable(self.unavailable), |v| {
                    v.state().clone()
                }),
            profile_revision: self.profile_revision,
            account_revision: self.epoch,
            checking: self.checking,
            refresh_needed: self
                .observer
                .as_ref()
                .is_some_and(AccountQuota::needs_refresh),
            last_attempt_at_ms: self.last_attempt_at_ms,
        }
    }
    /// A pump can copy observations before a later account invalidation in the
    /// same batch. Publish only the final current-owner view, after all Account
    /// events, so old buckets cannot be attributed to a replacement account.
    pub(crate) fn coalesce_events<T>(
        &self,
        events: &mut Vec<T>,
        is_quota: impl Fn(&T) -> bool,
        project: impl FnOnce(QuotaView) -> T,
    ) {
        let mut found = false;
        events.retain(|event| {
            let quota = is_quota(event);
            found |= quota;
            !quota
        });
        if found {
            events.push(project(self.view()));
        }
    }
    pub(crate) fn take_changed(&mut self) -> Option<QuotaView> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        Some(self.view())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn owner() -> QuotaOwner {
        QuotaOwner::new(
            Scope::new("synthetic-profile".into(), [1; 16], 7).unwrap(),
            7,
        )
    }
    fn reading(used: f64) -> Value {
        json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":used,"windowDurationMins":300,"resetsAt":500000},"secondary":null,"planType":"plus"}})
    }
    #[test]
    fn original_status_qualifier_accepts_only_current_empty_initial_projections() {
        use zephium_provider_accounts::codex::AccountState;
        let mut q = owner();
        q.invalidate(false); // Initial account/read admission invalidation.
        let checking = q.take_changed().unwrap();
        assert!(qualify_initial_quota(
            &q.scope,
            &q.scope,
            false,
            AccountState::Checking,
            &q.view(),
            &checking,
        ));
        assert!(q.begin(Instant::now(), 100).is_none());
        q.signed_out();
        let signed_out = q.take_changed().unwrap();
        assert!(qualify_initial_quota(
            &q.scope,
            &q.scope,
            false,
            AccountState::SignedOut,
            &q.view(),
            &signed_out,
        ));
        assert!(q.begin(Instant::now(), 100).is_none());
        assert!(!qualify_initial_quota(
            &q.scope,
            &q.scope,
            false,
            AccountState::SignedOut,
            &q.view(),
            &checking,
        ));
        assert!(!qualify_initial_quota(
            &q.scope,
            &q.scope,
            false,
            AccountState::Checking,
            &q.view(),
            &signed_out,
        ));
    }
    #[test]
    fn original_status_qualifier_refuses_foreign_or_observing_projections() {
        use zephium_provider_accounts::codex::AccountState;
        let mut q = owner();
        q.signed_out();
        let current = q.view();
        let foreign = Scope::new("synthetic-profile".into(), [2; 16], 7).unwrap();
        assert!(!qualify_initial_quota(
            &q.scope,
            &foreign,
            false,
            AccountState::SignedOut,
            &current,
            &current,
        ));
        assert!(!qualify_initial_quota(
            &q.scope,
            &q.scope,
            true,
            AccountState::SignedOut,
            &current,
            &current,
        ));
        assert!(!qualify_initial_quota(
            &q.scope,
            &q.scope,
            false,
            AccountState::ChatGpt,
            &current,
            &current,
        ));
        for changed in 0..6 {
            let mut observed = current.clone();
            match changed {
                0 => observed.profile_revision = 0,
                1 => observed.account_revision = 0,
                2 => observed.account_revision += 1,
                3 => observed.checking = true,
                4 => observed.refresh_needed = true,
                _ => observed.last_attempt_at_ms = Some(100),
            }
            assert!(!qualify_initial_quota(
                &q.scope,
                &q.scope,
                false,
                AccountState::SignedOut,
                &current,
                &observed,
            ));
            if changed != 2 {
                assert!(!qualify_initial_quota(
                    &q.scope,
                    &q.scope,
                    false,
                    AccountState::SignedOut,
                    &observed,
                    &observed,
                ));
            }
        }
        q.ready();
        let (epoch, ticket) = q.begin(Instant::now(), 100).unwrap();
        q.complete(epoch, ticket, &reading(10.0), 100);
        assert!(matches!(q.view().state, QuotaState::Available(_)));
        let observed = q.view();
        assert!(!qualify_initial_quota(
            &q.scope,
            &q.scope,
            false,
            AccountState::SignedOut,
            &observed,
            &observed,
        ));
        q.mark_stale(30_100);
        let observed = q.view();
        assert!(matches!(observed.state, QuotaState::Stale { .. }));
        assert!(!qualify_initial_quota(
            &q.scope,
            &q.scope,
            false,
            AccountState::SignedOut,
            &observed,
            &observed,
        ));
    }
    #[test]
    fn signed_out_never_reads_and_ready_has_one_initial_observation() {
        let mut q = owner();
        let now = Instant::now();
        assert!(q.begin(now, 100).is_none());
        q.signed_out();
        assert_eq!(
            q.view().state,
            QuotaState::Unavailable(QuotaUnavailable::SignedOut)
        );
        q.ready();
        let (e, t) = q.begin(now, 100).unwrap();
        assert!(q.begin(now, 100).is_none());
        q.complete(e, t, &reading(10.0), 100);
        assert!(matches!(q.view().state, QuotaState::Available(_)));
        assert!(q.begin(now + INTERVAL, 30100).is_none());
    }
    #[test]
    fn account_replacement_keeps_connection_cadence_and_discards_old_ticket() {
        let mut q = owner();
        q.ready();
        let now = Instant::now();
        let (old, t) = q.begin(now, 100).unwrap();
        q.invalidate(false);
        q.ready();
        assert_ne!(q.epoch(), old);
        q.complete(old, t, &reading(99.0), 200);
        assert!(matches!(q.view().state, QuotaState::Unavailable(_)));
        assert!(q.begin(now + Duration::from_secs(29), 29100).is_none());
        let (e, t) = q.begin(now + INTERVAL, 30100).unwrap();
        q.complete(e, t, &reading(5.0), 30100);
        assert!(matches!(q.view().state, QuotaState::Available(_)));
    }
    #[test]
    fn malformed_observation_retains_last_good_as_stale_without_auth_semantics() {
        let mut q = owner();
        q.ready();
        let now = Instant::now();
        let (e, t) = q.begin(now, 100).unwrap();
        q.complete(e, t, &reading(10.0), 100);
        q.manual();
        let (e, t) = q.begin(now + INTERVAL, 30100).unwrap();
        q.complete(e, t, &reading(101.0), 30100);
        assert!(matches!(
            q.view().state,
            QuotaState::Stale {
                fault: Some(QuotaFault::InvalidPercentage),
                ..
            }
        ));
        assert!(q.view().refresh_needed);
        assert!(!q.view().checking);
    }
    #[test]
    fn notification_storm_marks_stale_without_reads_or_budget_reset() {
        let mut q = owner();
        q.ready();
        let now = Instant::now();
        let (e, t) = q.begin(now, 100).unwrap();
        q.complete(e, t, &reading(10.0), 100);
        q.take_changed();
        let mut changes = 0;
        for _ in 0..1000 {
            q.updated(&reading(20.0), 200);
            changes += usize::from(q.take_changed().is_some());
            assert!(q.begin(now + Duration::from_secs(1), 200).is_none());
        }
        assert_eq!(changes, 1);
        assert_eq!(q.reads, 1);
        assert!(matches!(q.view().state, QuotaState::Stale { .. }));
        let (e, t) = q.begin(now + INTERVAL, 30100).unwrap();
        q.complete(e, t, &reading(20.0), 30100);
        assert_eq!(q.reads, 2);
    }
    #[test]
    fn elapsed_freshness_does_not_create_periodic_network_poll() {
        let mut q = owner();
        q.ready();
        let now = Instant::now();
        let (e, t) = q.begin(now, 100).unwrap();
        q.complete(e, t, &reading(10.0), 100);
        q.mark_stale(30100);
        assert!(matches!(
            q.view().state,
            QuotaState::Stale { fault: None, .. }
        ));
        assert!(q.begin(now + INTERVAL, 30100).is_none());
        q.manual();
        assert!(q.begin(now + INTERVAL, 30100).is_some());
    }
    #[test]
    fn timeout_and_provider_refusal_are_observational_and_retry_stays_bounded() {
        for fault in [QuotaFault::TimedOut, QuotaFault::ProviderRefused] {
            let mut q = owner();
            q.ready();
            let now = Instant::now();
            let (e, t) = q.begin(now, 100).unwrap();
            q.failed(e, t, fault, 200);
            assert_eq!(q.view().state, QuotaState::Error(fault));
            assert!(q.begin(now, 200).is_none());
            // Failure itself never causes periodic automatic retry.
            assert!(q.begin(now + INTERVAL, 30100).is_none());
            q.manual();
            assert!(q.begin(now + INTERVAL, 30100).is_some());
        }
    }
    #[test]
    fn connection_budget_cannot_be_refunded_by_replacing_account_observers() {
        let mut q = owner();
        let now = Instant::now();
        for i in 0..MAX_CONNECTION_READS {
            q.invalidate(false);
            q.ready();
            let (e, t) = q
                .begin(now + INTERVAL * u32::from(i), u64::from(i) * 30000)
                .unwrap();
            q.failed(e, t, QuotaFault::ProviderRefused, u64::from(i) * 30000);
        }
        q.invalidate(false);
        q.ready();
        assert!(q
            .begin(now + INTERVAL * u32::from(MAX_CONNECTION_READS), 20000000)
            .is_none());
        assert_eq!(q.reads, MAX_CONNECTION_READS);
    }
    #[test]
    fn retired_epoch_exhaustion_stays_closed_and_projection_updates_are_coalescible() {
        let mut q = owner();
        assert!(q.take_changed().is_some());
        assert!(q.take_changed().is_none());
        q.epoch = u64::MAX;
        q.invalidate(false);
        q.ready();
        q.invalidate(false);
        q.ready();
        assert_eq!(q.epoch(), 0);
        assert!(q.begin(Instant::now(), 100).is_none());
        assert!(q.take_changed().is_some());
        assert!(q.take_changed().is_none());
    }
    #[test]
    fn signedout_read_itself_retires_values_and_late_pending_observation() {
        let mut q = owner();
        q.ready();
        let now = Instant::now();
        let (epoch, ticket) = q.begin(now, 100).unwrap();
        q.complete(epoch, ticket, &reading(10.0), 100);
        q.manual();
        let (epoch, ticket) = q.begin(now + INTERVAL, 30100).unwrap();
        q.signed_out();
        assert_eq!(
            q.view().state,
            QuotaState::Unavailable(QuotaUnavailable::SignedOut)
        );
        q.complete(epoch, ticket, &reading(99.0), 30200);
        assert_eq!(
            q.view().state,
            QuotaState::Unavailable(QuotaUnavailable::SignedOut)
        );
        assert!(!q.view().checking);
        assert!(q.begin(now + INTERVAL * 2, 60100).is_none());
        assert_eq!(q.reads, 2);
    }
    #[test]
    fn copied_old_quota_then_invalidation_and_fresh_account_coalesce_to_current_epoch() {
        #[derive(Debug, PartialEq)]
        enum Event {
            Account(&'static str),
            Quota(QuotaView),
        }
        let mut q = owner();
        q.ready();
        let (epoch, ticket) = q.begin(Instant::now(), 100).unwrap();
        q.complete(epoch, ticket, &reading(90.0), 101);
        let old = q.view();
        assert!(matches!(old.state, QuotaState::Available(_)));
        let mut events = vec![Event::Quota(old.clone()), Event::Account("unknown")];
        q.invalidate(false);
        q.ready();
        events.push(Event::Account("chatgpt"));
        // Simulate the second copied observation retained in the same queue.
        events.push(Event::Quota(old));
        q.coalesce_events(
            &mut events,
            |event| matches!(event, Event::Quota(_)),
            Event::Quota,
        );
        assert_eq!(
            events,
            vec![
                Event::Account("unknown"),
                Event::Account("chatgpt"),
                Event::Quota(q.view())
            ]
        );
        assert_ne!(q.view().account_revision, epoch);
        assert!(matches!(q.view().state, QuotaState::Unavailable(_)));
        let mut quiet: Vec<Event> = Vec::new();
        q.coalesce_events(
            &mut quiet,
            |event| matches!(event, Event::Quota(_)),
            Event::Quota,
        );
        assert!(quiet.is_empty());
    }
}
