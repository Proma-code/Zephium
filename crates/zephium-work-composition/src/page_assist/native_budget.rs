//! One reply's cumulative native-browser budget. Only Page's actual human-wait
//! sites own transition authority; the native actor receives a read-only view.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

const ACTIVE: Duration = Duration::from_secs(90);
const HUMAN: Duration = Duration::from_secs(300);
const TOTAL: Duration = Duration::from_secs(390);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeBudgetFault {
    Expired,
    InvalidTransition,
    ClockRegressed,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HumanWaitKind {
    Confirmation,
    Takeover,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Active,
    Human { ticket: u64, kind: HumanWaitKind },
}

struct State {
    start: Instant,
    last: Instant,
    active: Duration,
    human: Duration,
    phase: Phase,
    next_ticket: u64,
    fault: Option<NativeBudgetFault>,
}

impl State {
    fn refuse<T>(&mut self, fault: NativeBudgetFault) -> Result<T, NativeBudgetFault> {
        let first = *self.fault.get_or_insert(fault);
        Err(first)
    }

    fn advance(&mut self, now: Instant) -> Result<(), NativeBudgetFault> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let Some(elapsed) = now.checked_duration_since(self.last) else {
            return self.refuse(NativeBudgetFault::ClockRegressed);
        };
        match self.phase {
            Phase::Active => self.active = self.active.saturating_add(elapsed),
            Phase::Human { .. } => self.human = self.human.saturating_add(elapsed),
        }
        self.last = now;
        if now.duration_since(self.start) >= TOTAL || self.active >= ACTIVE || self.human >= HUMAN {
            return self.refuse(NativeBudgetFault::Expired);
        }
        Ok(())
    }

    fn deadline(&mut self, now: Instant) -> Result<Instant, NativeBudgetFault> {
        self.advance(now)?;
        let remaining = match self.phase {
            Phase::Active => ACTIVE - self.active,
            Phase::Human { .. } => HUMAN - self.human,
        };
        let Some(phase_deadline) = now.checked_add(remaining) else {
            return self.refuse(NativeBudgetFault::InvalidTransition);
        };
        let Some(absolute) = self.start.checked_add(TOTAL) else {
            return self.refuse(NativeBudgetFault::InvalidTransition);
        };
        Ok(phase_deadline.min(absolute))
    }
}

struct Shared {
    state: Mutex<State>,
    changed: watch::Sender<()>,
}

impl Shared {
    fn with<T>(
        &self,
        f: impl FnOnce(&mut State) -> Result<T, NativeBudgetFault>,
    ) -> Result<T, NativeBudgetFault> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                let result = state.refuse(NativeBudgetFault::InvalidTransition);
                drop(state);
                self.changed.send_replace(());
                return result;
            }
        };
        let result = f(&mut state);
        drop(state);
        if result.is_err() {
            self.changed.send_replace(());
        }
        result
    }

    fn seal(&self, fault: NativeBudgetFault) {
        let _: Result<(), _> = self.with(|state| state.refuse(fault));
        self.changed.send_replace(());
    }
}

/// A clone is observation only: it shares the original budget and cannot pause,
/// replenish, replace its start instant, or reopen a sealed reply.
#[derive(Clone)]
pub struct NativeBudgetView {
    shared: Arc<Shared>,
    changed: watch::Receiver<()>,
}

impl NativeBudgetView {
    /// Recheck before native dispatch and after every wake. The returned deadline
    /// can move only because Page entered/exited an actual bounded human wait.
    pub fn deadline(&self) -> Result<Instant, NativeBudgetFault> {
        self.shared.with(|state| state.deadline(now()))
    }

    /// Original unconditional ceiling, shared with the live cumulative deadline.
    /// This observation cannot renew the start or grant a human-wait transition.
    /// Native writers must still read `deadline()` immediately before first byte.
    pub fn hard_deadline(&self) -> Result<Instant, NativeBudgetFault> {
        self.shared.with(|state| {
            if let Some(fault) = state.fault {
                return Err(fault);
            }
            match state.start.checked_add(TOTAL) {
                Some(deadline) => Ok(deadline),
                None => state.refuse(NativeBudgetFault::InvalidTransition),
            }
        })
    }

    #[cfg(test)]
    fn deadline_at(&self, now: Instant) -> Result<Instant, NativeBudgetFault> {
        self.shared.with(|state| state.deadline(now))
    }

    /// Wakes on wait transitions or irreversible seal. The watch cursor avoids
    /// a lost wake between a deadline check and arming the actor's wait.
    pub async fn changed(&mut self) {
        let _ = self.changed.changed().await;
    }
}

/// Sole Page-owned transition authority. Dropping it closes all native views.
pub(super) struct NativeBudget {
    shared: Arc<Shared>,
}

fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

impl NativeBudget {
    pub(super) fn new() -> Self {
        Self::new_at(now())
    }

    fn new_at(start: Instant) -> Self {
        let (changed, _) = watch::channel(());
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    start,
                    last: start,
                    active: Duration::ZERO,
                    human: Duration::ZERO,
                    phase: Phase::Active,
                    next_ticket: 1,
                    fault: None,
                }),
                changed,
            }),
        }
    }

    pub(super) fn view(&self) -> NativeBudgetView {
        NativeBudgetView {
            shared: self.shared.clone(),
            changed: self.shared.changed.subscribe(),
        }
    }

    pub(super) fn wait(&self, kind: HumanWaitKind) -> Result<NativeHumanWait, NativeBudgetFault> {
        self.wait_with(kind, now)
    }

    #[cfg(test)]
    fn wait_at(
        &self,
        kind: HumanWaitKind,
        instant: Instant,
    ) -> Result<NativeHumanWait, NativeBudgetFault> {
        self.wait_with(kind, || instant)
    }

    fn wait_with(
        &self,
        kind: HumanWaitKind,
        clock: impl FnOnce() -> Instant,
    ) -> Result<NativeHumanWait, NativeBudgetFault> {
        let ticket = self.shared.with(|state| {
            state.advance(clock())?;
            if state.phase != Phase::Active {
                return state.refuse(NativeBudgetFault::InvalidTransition);
            }
            let ticket = state.next_ticket;
            let Some(next) = ticket.checked_add(1) else {
                return state.refuse(NativeBudgetFault::InvalidTransition);
            };
            state.next_ticket = next;
            state.phase = Phase::Human { ticket, kind };
            Ok(ticket)
        })?;
        self.shared.changed.send_replace(());
        Ok(NativeHumanWait {
            shared: self.shared.clone(),
            ticket,
            kind,
            finished: false,
        })
    }

    pub(super) fn seal(&self) {
        self.shared.seal(NativeBudgetFault::Closed);
    }
}

impl Drop for NativeBudget {
    fn drop(&mut self) {
        self.seal();
    }
}

/// Non-Clone exact wait receipt. Abandonment (including cancellation/unwind)
/// seals admission; it never silently resumes active work or refunds time.
pub(super) struct NativeHumanWait {
    shared: Arc<Shared>,
    ticket: u64,
    kind: HumanWaitKind,
    finished: bool,
}

impl NativeHumanWait {
    pub(super) fn finish(self) -> Result<(), NativeBudgetFault> {
        self.finish_with(now)
    }

    #[cfg(test)]
    fn finish_at(self, instant: Instant) -> Result<(), NativeBudgetFault> {
        self.finish_with(|| instant)
    }

    fn finish_with(mut self, clock: impl FnOnce() -> Instant) -> Result<(), NativeBudgetFault> {
        let result = self.shared.with(|state| {
            state.advance(clock())?;
            if state.phase
                != (Phase::Human {
                    ticket: self.ticket,
                    kind: self.kind,
                })
            {
                return state.refuse(NativeBudgetFault::InvalidTransition);
            }
            state.phase = Phase::Active;
            Ok(())
        });
        self.finished = true;
        self.shared.changed.send_replace(());
        result
    }
}

impl Drop for NativeHumanWait {
    fn drop(&mut self) {
        if !self.finished {
            self.shared.seal(NativeBudgetFault::InvalidTransition);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn active_deadline_never_restarts_when_observed_or_cloned() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let view = budget.view();
        assert_eq!(view.deadline_at(at(start, 40)), Ok(at(start, 90)));
        assert_eq!(view.clone().deadline_at(at(start, 89)), Ok(at(start, 90)));
        assert_eq!(
            view.deadline_at(at(start, 90)),
            Err(NativeBudgetFault::Expired)
        );
    }

    #[test]
    fn separate_waits_share_active_and_human_allowances() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let first = budget
            .wait_at(HumanWaitKind::Confirmation, at(start, 20))
            .unwrap();
        first.finish_at(at(start, 120)).unwrap();
        let second = budget
            .wait_at(HumanWaitKind::Takeover, at(start, 150))
            .unwrap();
        assert_eq!(
            budget.view().deadline_at(at(start, 200)),
            Ok(at(start, 350))
        );
        second.finish_at(at(start, 349)).unwrap();
        assert_eq!(
            budget.view().deadline_at(at(start, 350)),
            Ok(at(start, 389))
        );
        assert_eq!(
            budget.view().deadline_at(at(start, 389)),
            Err(NativeBudgetFault::Expired)
        );
    }

    #[test]
    fn cumulative_human_limit_is_not_per_wait() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        budget
            .wait_at(HumanWaitKind::Confirmation, start)
            .unwrap()
            .finish_at(at(start, 200))
            .unwrap();
        let wait = budget
            .wait_at(HumanWaitKind::Takeover, at(start, 201))
            .unwrap();
        assert_eq!(
            wait.finish_at(at(start, 301)),
            Err(NativeBudgetFault::Expired)
        );
    }

    #[test]
    fn absolute_ceiling_survives_an_outstanding_wait() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let _wait = budget
            .wait_at(HumanWaitKind::Confirmation, at(start, 89))
            .unwrap();
        assert!(budget.view().deadline_at(at(start, 388)).unwrap() <= at(start, 390));
        assert_eq!(
            budget.view().deadline_at(at(start, 390)),
            Err(NativeBudgetFault::Expired)
        );
    }

    #[test]
    fn nested_wait_is_sticky_refusal() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let first = budget.wait_at(HumanWaitKind::Confirmation, start).unwrap();
        assert!(matches!(
            budget.wait_at(HumanWaitKind::Takeover, at(start, 1)),
            Err(NativeBudgetFault::InvalidTransition)
        ));
        assert_eq!(
            first.finish_at(at(start, 2)),
            Err(NativeBudgetFault::InvalidTransition)
        );
    }

    #[test]
    fn dropped_wait_never_reopens_active_admission() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        drop(budget.wait_at(HumanWaitKind::Confirmation, start).unwrap());
        assert_eq!(
            budget.view().deadline_at(at(start, 1)),
            Err(NativeBudgetFault::InvalidTransition)
        );
    }

    #[test]
    fn clock_rollback_fault_survives_later_forward_time() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let view = budget.view();
        view.deadline_at(at(start, 20)).unwrap();
        assert_eq!(
            view.deadline_at(at(start, 19)),
            Err(NativeBudgetFault::ClockRegressed)
        );
        assert_eq!(
            view.deadline_at(at(start, 21)),
            Err(NativeBudgetFault::ClockRegressed)
        );
    }

    #[test]
    fn owner_drop_seals_every_retained_view() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let view = budget.view();
        drop(budget);
        assert_eq!(view.deadline_at(start), Err(NativeBudgetFault::Closed));
    }

    #[test]
    fn explicit_seal_cannot_be_undone_by_wait_completion() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let wait = budget.wait_at(HumanWaitKind::Takeover, start).unwrap();
        budget.seal();
        assert_eq!(wait.finish_at(at(start, 1)), Err(NativeBudgetFault::Closed));
    }

    #[tokio::test(start_paused = true)]
    async fn actor_change_cursor_cannot_miss_wait_end() {
        let budget = NativeBudget::new();
        let mut view = budget.view();
        let wait = budget.wait(HumanWaitKind::Confirmation).unwrap();
        view.changed().await;
        tokio::time::advance(Duration::from_secs(20)).await;
        wait.finish().unwrap();
        view.changed().await;
        assert_eq!(view.deadline().unwrap(), now() + ACTIVE);
        budget.seal();
        view.changed().await;
        assert_eq!(view.deadline(), Err(NativeBudgetFault::Closed));
    }

    #[test]
    fn hard_ceiling_is_original_across_shared_wait_transitions() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let actor = budget.view();
        let writer = actor.clone();
        assert_eq!(actor.hard_deadline().unwrap(), at(start, 390));
        let wait = budget
            .wait_at(HumanWaitKind::Confirmation, at(start, 80))
            .unwrap();
        assert_eq!(writer.hard_deadline().unwrap(), at(start, 390));
        assert_eq!(writer.deadline_at(at(start, 95)).unwrap(), at(start, 380));
        wait.finish_at(at(start, 100)).unwrap();
        assert_eq!(writer.deadline_at(at(start, 100)).unwrap(), at(start, 110));
        assert_eq!(writer.hard_deadline().unwrap(), at(start, 390));
        budget.seal();
        assert_eq!(writer.hard_deadline(), Err(NativeBudgetFault::Closed));
    }

    #[test]
    fn live_writer_read_refuses_an_expired_former_human_wait_extension() {
        let start = Instant::now();
        let budget = NativeBudget::new_at(start);
        let writer = budget.view();
        let wait = budget
            .wait_at(HumanWaitKind::Takeover, at(start, 80))
            .unwrap();
        let queued_deadline = writer.deadline_at(at(start, 90)).unwrap();
        wait.finish_at(at(start, 100)).unwrap();
        let first_byte = at(start, 111);
        assert!(
            first_byte < queued_deadline,
            "the queued copy alone would incorrectly admit"
        );
        let mut writes = 0;
        if writer
            .deadline_at(first_byte)
            .is_ok_and(|deadline| first_byte < deadline)
        {
            writes += 1;
        }
        assert_eq!(
            writes, 0,
            "final native handoff must consult the same live budget"
        );
    }
}
