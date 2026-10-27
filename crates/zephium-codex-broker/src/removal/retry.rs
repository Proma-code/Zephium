//! Distinct explicit local reconciliation action. It cannot recreate an original
//! logout/transport receipt, increment a grant, start a process or resend a request.
use super::{Fault, RemovalAdmission};
use crate::profile::{removal::RemovalAttemptId, ProfileId};
use std::time::{Duration, Instant};
const LOCAL_SLICE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovalRetryFault {
    Scope,
    Interrupted,
    Expired,
}
/// Mint only after the native participant joins an explicit app Retry action to
/// the same verified protected Prepared operation. No Clone/serde/reset API.
pub struct RemovalRetryAdmission {
    profile: ProfileId,
    attempt: RemovalAttemptId,
    incarnation: [u8; 16],
    retired_epoch: u64,
    admission: RemovalAdmission,
    deadline: Instant,
}
impl RemovalRetryAdmission {
    pub fn from_native_retry(
        profile: ProfileId,
        attempt: RemovalAttemptId,
        incarnation: [u8; 16],
        retired_epoch: u64,
        admission: RemovalAdmission,
    ) -> Result<Self, RemovalRetryFault> {
        Self::new_at(
            profile,
            attempt,
            incarnation,
            retired_epoch,
            admission,
            Instant::now(),
        )
    }
    fn new_at(
        profile: ProfileId,
        attempt: RemovalAttemptId,
        incarnation: [u8; 16],
        retired_epoch: u64,
        admission: RemovalAdmission,
        now: Instant,
    ) -> Result<Self, RemovalRetryFault> {
        admission.current().map_err(action_fault)?;
        if incarnation == [0; 16] || !(2..=i64::MAX as u64).contains(&retired_epoch) {
            return Err(RemovalRetryFault::Scope);
        }
        Ok(Self {
            profile,
            attempt,
            incarnation,
            retired_epoch,
            admission,
            deadline: now + LOCAL_SLICE,
        })
    }
    pub(crate) fn current(
        &self,
        profile: ProfileId,
        attempt: RemovalAttemptId,
        incarnation: [u8; 16],
        retired_epoch: u64,
    ) -> Result<(), RemovalRetryFault> {
        self.admission.current().map_err(action_fault)?;
        if Instant::now() >= self.deadline {
            return Err(RemovalRetryFault::Expired);
        }
        if self.profile != profile
            || self.attempt != attempt
            || self.incarnation != incarnation
            || self.retired_epoch != retired_epoch
        {
            return Err(RemovalRetryFault::Scope);
        }
        Ok(())
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
}
fn action_fault(fault: Fault) -> RemovalRetryFault {
    match fault {
        Fault::Cancelled => RemovalRetryFault::Interrupted,
        _ => RemovalRetryFault::Scope,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    };
    fn profile() -> ProfileId {
        ProfileId::from_native([1; 16]).unwrap()
    }
    fn attempt() -> RemovalAttemptId {
        RemovalAttemptId::from_native_bytes([2; 16]).unwrap()
    }
    fn action(start: Instant) -> (RemovalRetryAdmission, Arc<AtomicBool>, Arc<AtomicU64>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(3));
        let admission =
            RemovalAdmission::from_native(cancel.clone(), generation.clone(), 3).unwrap();
        (
            RemovalRetryAdmission::new_at(profile(), attempt(), [4; 16], 5, admission, start)
                .unwrap(),
            cancel,
            generation,
        )
    }
    #[test]
    fn retry_is_exact_same_prepared_operation_and_own_fixed_local_slice() {
        let now = Instant::now();
        let (a, _, _) = action(now);
        assert_eq!(a.deadline(), now + Duration::from_secs(10));
        assert_eq!(a.current(profile(), attempt(), [4; 16], 5), Ok(()));
        assert_eq!(
            a.current(
                ProfileId::from_native([7; 16]).unwrap(),
                attempt(),
                [4; 16],
                5
            ),
            Err(RemovalRetryFault::Scope)
        );
        assert_eq!(
            a.current(
                profile(),
                RemovalAttemptId::from_native_bytes([7; 16]).unwrap(),
                [4; 16],
                5
            ),
            Err(RemovalRetryFault::Scope)
        );
        assert_eq!(
            a.current(profile(), attempt(), [7; 16], 5),
            Err(RemovalRetryFault::Scope)
        );
        assert_eq!(
            a.current(profile(), attempt(), [4; 16], 6),
            Err(RemovalRetryFault::Scope)
        );
    }
    #[test]
    fn retry_expiry_cancel_or_native_replacement_cannot_be_rearmed() {
        let (a, _, _) = action(Instant::now() - Duration::from_secs(11));
        assert_eq!(
            a.current(profile(), attempt(), [4; 16], 5),
            Err(RemovalRetryFault::Expired)
        );
        let (a, cancel, _) = action(Instant::now());
        cancel.store(true, Ordering::Release);
        assert_eq!(
            a.current(profile(), attempt(), [4; 16], 5),
            Err(RemovalRetryFault::Interrupted)
        );
        let (a, _, generation) = action(Instant::now());
        generation.fetch_add(1, Ordering::AcqRel);
        assert_eq!(
            a.current(profile(), attempt(), [4; 16], 5),
            Err(RemovalRetryFault::Scope)
        );
    }
    #[test]
    fn retry_fields_do_not_accept_unretired_or_missing_identity() {
        for (incarnation, epoch) in [
            ([0; 16], 5),
            ([4; 16], 0),
            ([4; 16], 1),
            ([4; 16], u64::MAX),
        ] {
            let admission = RemovalAdmission::from_native(
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU64::new(3)),
                3,
            )
            .unwrap();
            assert_eq!(
                RemovalRetryAdmission::from_native_retry(
                    profile(),
                    attempt(),
                    incarnation,
                    epoch,
                    admission
                )
                .err(),
                Some(RemovalRetryFault::Scope)
            );
        }
    }
}
