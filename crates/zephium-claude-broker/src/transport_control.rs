//! Borrowed native cancellation/deadline fence; no detached forwarding thread.
use crate::account::Control;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub(crate) enum Fence<'a> {
    Atomic(&'a AtomicBool),
    Admitted(&'a Control),
}
impl Fence<'_> {
    pub(crate) fn cancelled(&self, now: Instant) -> bool {
        match self {
            Self::Atomic(flag) => flag.load(Ordering::Acquire),
            Self::Admitted(control) => control.cancelled(now),
        }
    }
    pub(crate) fn bound(&self, proposed: Instant) -> Instant {
        match self {
            Self::Atomic(_) => proposed,
            Self::Admitted(control) => proposed.min(control.deadline()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        account::{AccountOwner, Error, Reply, Request},
        custody::Owner,
        protocol::AuthStatus,
    };
    use std::{sync::atomic::AtomicU64, time::Duration};
    fn owner() -> AccountOwner {
        static NEXT: AtomicU64 = AtomicU64::new(1 << 48);
        AccountOwner::new(Owner {
            profile: NEXT.fetch_add(1, Ordering::Relaxed).into(),
            account_generation: 2,
            profile_generation: 3,
        })
        .unwrap()
    }
    #[test]
    fn atomic_control_preserves_existing_deadline_and_observes_cancel() {
        let flag = AtomicBool::new(false);
        let fence = Fence::Atomic(&flag);
        let deadline = Instant::now();
        assert_eq!(fence.bound(deadline), deadline);
        assert!(!fence.cancelled(deadline));
        flag.store(true, Ordering::Release);
        assert!(fence.cancelled(deadline));
    }
    #[test]
    fn preparation_never_restarts_the_account_budget() {
        let owner = owner();
        let now = Instant::now();
        let work = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        let result = work.run(
            &mut |_: Request, control: &Control| {
                let fence = Fence::Admitted(control);
                assert_eq!(
                    fence.bound(control.deadline() + Duration::from_secs(9)),
                    control.deadline()
                );
                assert_eq!(
                    fence.bound(now + Duration::from_secs(1)),
                    now + Duration::from_secs(1)
                );
                assert!(!fence.cancelled(now));
                assert!(fence.cancelled(control.deadline()));
                assert!(fence.cancelled(control.deadline() + Duration::from_secs(1)));
                Ok(Reply::Status(AuthStatus::SignedOut))
            },
            || now,
        );
        assert_eq!(result, Ok(Reply::Status(AuthStatus::SignedOut)));
    }
    #[test]
    fn admitted_cancellation_is_observed_without_a_relay() {
        let owner = owner();
        let now = Instant::now();
        let work = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        let result = work.run(
            &mut |_: Request, control: &Control| {
                let fence = Fence::Admitted(control);
                assert!(!fence.cancelled(now));
                owner.close();
                assert!(fence.cancelled(now));
                Err(Error::Cancelled)
            },
            || now,
        );
        assert_eq!(result, Err(Error::Closed));
        assert!(owner.drained());
    }
}
