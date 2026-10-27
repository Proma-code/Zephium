//! Observational quiescence only. Neither this snapshot nor a true predicate is
//! a physical-close receipt, permission to replace an image, or wake authority.
pub(crate) fn same_home(expected: &crate::profile::Record, held: &crate::profile::Record) -> bool {
    expected == held
}
/// Retire only the original wake's historical correlation after exact-owner
/// physical closure. This does not attest CLEAN, graceful exit or current auth.
pub(crate) fn take_failed_wake<T>(
    origin: &mut Option<T>,
    current_scope: bool,
    physically_closed: bool,
    still_waking: bool,
) -> Option<T> {
    (current_scope && physically_closed && still_waking)
        .then(|| origin.take())
        .flatten()
}
pub(crate) struct Quiescence {
    pub(crate) current: bool,
    pub(crate) persistent: bool,
    pub(crate) account_stable: bool,
    pub(crate) account_idle: bool,
    pub(crate) pending: usize,
    pub(crate) expired_quota: usize,
    pub(crate) quota_idle: bool,
    pub(crate) active: bool,
    pub(crate) prompt: bool,
    pub(crate) catalog: bool,
    pub(crate) login: bool,
    pub(crate) closing: bool,
    pub(crate) removal: bool,
    pub(crate) browser: bool,
    pub(crate) wake: bool,
    pub(crate) queued_facts: usize,
    pub(crate) threads_closed: bool,
}
impl Quiescence {
    pub(crate) fn ready(&self) -> bool {
        self.current
            && self.persistent
            && self.account_stable
            && self.account_idle
            && self.pending == self.expired_quota
            && self.quota_idle
            && !self.active
            && !self.prompt
            && !self.catalog
            && !self.login
            && !self.closing
            && !self.removal
            && !self.browser
            && !self.wake
            && self.queued_facts == 0
            && self.threads_closed
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_wake_history_requires_exact_closed_wake_and_is_consumed_once() {
        let mut origin = Some(7);
        for (current, closed, waking) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            assert_eq!(take_failed_wake(&mut origin, current, closed, waking), None);
            assert_eq!(origin, Some(7));
        }
        assert_eq!(take_failed_wake(&mut origin, true, true, true), Some(7));
        assert_eq!(take_failed_wake(&mut origin, true, true, true), None);
        let mut ordinary_or_successful_owner: Option<u64> = None;
        assert_eq!(
            take_failed_wake(&mut ordinary_or_successful_owner, true, true, true),
            None
        );
    }
    fn ready() -> Quiescence {
        Quiescence {
            current: true,
            persistent: true,
            account_stable: true,
            account_idle: true,
            pending: 0,
            expired_quota: 0,
            quota_idle: true,
            active: false,
            prompt: false,
            catalog: false,
            login: false,
            closing: false,
            removal: false,
            browser: false,
            wake: false,
            queued_facts: 0,
            threads_closed: true,
        }
    }
    #[test]
    fn every_live_obligation_blocks_dormancy_independently() {
        assert!(ready().ready());
        let mutations: [fn(&mut Quiescence); 16] = [
            |s| s.current = false,
            |s| s.persistent = false,
            |s| s.account_stable = false,
            |s| s.account_idle = false,
            |s| s.pending = 1,
            |s| s.quota_idle = false,
            |s| s.active = true,
            |s| s.prompt = true,
            |s| s.catalog = true,
            |s| s.login = true,
            |s| s.closing = true,
            |s| s.removal = true,
            |s| s.browser = true,
            |s| s.wake = true,
            |s| s.queued_facts = 1,
            |s| s.threads_closed = false,
        ];
        for mutation in mutations {
            let mut snapshot = ready();
            mutation(&mut snapshot);
            assert!(!snapshot.ready());
        }
    }
    #[test]
    fn only_explicitly_classified_expired_quota_can_enter_physical_drain() {
        let mut snapshot = ready();
        // Every actual pending global ID is counted, even an expired quota's
        // tombstone. No observation timeout is reclassified as a drained reply.
        snapshot.pending = 1;
        assert!(!snapshot.ready());
        snapshot.expired_quota = 1;
        assert!(snapshot.ready());
        snapshot.pending = 0;
        snapshot.expired_quota = 0;
        snapshot.queued_facts = 1;
        assert!(!snapshot.ready());
        snapshot.queued_facts = 0;
        assert!(snapshot.ready());
    }
}
