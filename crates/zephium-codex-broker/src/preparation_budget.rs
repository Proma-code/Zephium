//! Logical startup timing, not process or account authority. Physical custody
//! still drains after expiry; no later stage may renew a removal obligation.
use std::time::{Duration, Instant};
const PREPARATION: Duration = Duration::from_secs(120);
const RPC: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub(crate) struct PreparationBudget {
    stage: Instant,
    removal_limit: Option<Instant>,
}
impl PreparationBudget {
    pub(crate) fn ordinary(deadline: Instant) -> Self {
        Self {
            stage: deadline,
            removal_limit: None,
        }
    }
    pub(crate) fn removal(now: Instant, original_limit: Instant) -> Self {
        let limit = (now + PREPARATION).min(original_limit);
        Self {
            stage: limit,
            removal_limit: Some(limit),
        }
    }
    pub(crate) fn deadline(self) -> Instant {
        self.stage
    }
    pub(crate) fn next_rpc(self, now: Instant) -> Self {
        let stage = now + RPC;
        Self {
            stage: self.removal_limit.map_or(stage, |limit| stage.min(limit)),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_queue_image_preparation_bootstrap_and_read_never_renew_removal() {
        let start = Instant::now();
        let original = start + PREPARATION;
        let queued = PreparationBudget::removal(start + Duration::from_secs(90), original);
        assert_eq!(queued.deadline(), original);
        let bootstrap = queued.next_rpc(start + Duration::from_secs(110));
        assert_eq!(bootstrap.deadline(), original);
        let read = bootstrap.next_rpc(start + Duration::from_secs(119));
        assert_eq!(read.deadline(), original);
        let after_expiry = read.next_rpc(start + Duration::from_secs(121));
        assert_eq!(after_expiry.deadline(), original);
        assert!(after_expiry.deadline() < start + Duration::from_secs(121));
    }
    #[test]
    fn short_initial_cap_and_expired_input_remain_exact() {
        let now = Instant::now();
        let short = now + Duration::from_secs(1);
        assert_eq!(
            PreparationBudget::removal(now, short)
                .next_rpc(now)
                .deadline(),
            short
        );
        let expired = now - Duration::from_secs(1);
        assert_eq!(PreparationBudget::removal(now, expired).deadline(), expired);
        assert_eq!(
            PreparationBudget::removal(now, now + Duration::from_secs(600)).deadline(),
            now + PREPARATION
        );
        assert_eq!(
            PreparationBudget::removal(now, now + Duration::from_secs(600))
                .next_rpc(now + Duration::from_secs(119))
                .deadline(),
            now + PREPARATION
        );
    }
    #[test]
    fn ordinary_connection_keeps_original_separate_rpc_allowances() {
        let now = Instant::now();
        let ordinary = PreparationBudget::ordinary(now + PREPARATION);
        let bootstrap = ordinary.next_rpc(now + Duration::from_secs(119));
        assert_eq!(bootstrap.deadline(), now + Duration::from_secs(179));
        assert_eq!(
            bootstrap
                .next_rpc(now + Duration::from_secs(178))
                .deadline(),
            now + Duration::from_secs(238)
        );
    }
}
