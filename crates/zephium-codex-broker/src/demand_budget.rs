//! Immutable native action timing/cancellation, never an RPC or account grant.
use crate::conversation::Scope;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const MAX_ACTION: Duration = Duration::from_secs(120);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    Scope,
    Cancelled,
    Expired,
}
#[derive(Clone)]
pub(crate) struct DemandBudget {
    scope: Scope,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}
impl DemandBudget {
    pub(crate) fn new(
        scope: Scope,
        cancel: Arc<AtomicBool>,
        original: Instant,
        now: Instant,
    ) -> Result<Self, Fault> {
        let value = Self {
            scope,
            cancel,
            deadline: original.min(now + MAX_ACTION),
        };
        value.check(&value.scope, now)?;
        Ok(value)
    }
    /// Absence is permitted only when BOTH fields are absent. A retired or
    /// malformed retained wake can never degrade into an ordinary unbounded read.
    pub(crate) fn from_wake(
        scope: Scope,
        cancel: Option<&Arc<AtomicBool>>,
        deadline: Option<Instant>,
        now: Instant,
    ) -> Result<Option<Self>, Fault> {
        match (cancel, deadline) {
            (None, None) => Ok(None),
            (Some(cancel), Some(deadline)) => {
                Self::new(scope, cancel.clone(), deadline, now).map(Some)
            }
            _ => Err(Fault::Scope),
        }
    }
    pub(crate) fn check(&self, scope: &Scope, now: Instant) -> Result<(), Fault> {
        if scope != &self.scope {
            return Err(Fault::Scope);
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err(Fault::Cancelled);
        }
        if now >= self.deadline {
            return Err(Fault::Expired);
        }
        Ok(())
    }
    pub(crate) fn cap(&self, other: Instant) -> Instant {
        other.min(self.deadline)
    }
    pub(crate) fn cancel(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scope(n: u64) -> Scope {
        Scope::new("profile".into(), [1; 16], n).unwrap()
    }
    #[test]
    fn retired_or_partial_wake_never_falls_back_to_unfenced_observation() {
        let now = Instant::now();
        let cancel = Arc::new(AtomicBool::new(false));
        let limit = now + MAX_ACTION;
        assert!(DemandBudget::from_wake(scope(1), None, None, now)
            .unwrap()
            .is_none());
        assert!(
            DemandBudget::from_wake(scope(1), Some(&cancel), Some(limit), now)
                .unwrap()
                .is_some()
        );
        cancel.store(true, Ordering::Release);
        assert!(matches!(
            DemandBudget::from_wake(scope(1), Some(&cancel), Some(limit), now),
            Err(Fault::Cancelled)
        ));
        assert!(matches!(
            DemandBudget::from_wake(scope(1), None, Some(limit), now),
            Err(Fault::Scope)
        ));
        assert!(matches!(
            DemandBudget::from_wake(scope(1), Some(&cancel), None, now),
            Err(Fault::Scope)
        ));
        assert!(matches!(
            DemandBudget::from_wake(
                scope(1),
                Some(&Arc::new(AtomicBool::new(false))),
                Some(now),
                now
            ),
            Err(Fault::Expired)
        ));
    }
    #[test]
    fn queue_wake_rpc_and_cadence_never_renew_original_action() {
        let start = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let original = start + MAX_ACTION;
        let action =
            DemandBudget::new(scope(1), stop, original, start + Duration::from_secs(110)).unwrap();
        assert_eq!(action.cap(start + Duration::from_secs(170)), original);
        assert!(action.check(&scope(1), original).is_err());
        assert!(action
            .clone()
            .check(&scope(1), original + Duration::from_secs(30))
            .is_err());
        assert!(DemandBudget::new(
            scope(1),
            Arc::new(AtomicBool::new(false)),
            original,
            original
        )
        .is_err());
    }
    #[test]
    fn exact_action_cancel_and_scope_survive_copies_and_final_gate() {
        let now = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let action = DemandBudget::new(scope(1), stop.clone(), now + MAX_ACTION, now).unwrap();
        let queued = action.clone();
        assert!(queued.check(&scope(1), now).is_ok());
        assert_eq!(queued.check(&scope(2), now), Err(Fault::Scope));
        stop.store(true, Ordering::Release);
        assert_eq!(queued.check(&scope(1), now), Err(Fault::Cancelled));
        assert!(queued.cancel().load(Ordering::Acquire));
    }
}
