//! Local queue capacity and move-only turn ownership. No provider authority.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Busy,
    Closed,
}
struct State {
    gate: Mutex<()>,
    capacity: usize,
    queue: Arc<Semaphore>,
    turn: Arc<Semaphore>,
    browser_turn: Arc<AtomicBool>,
    ready: AtomicBool,
}
pub(super) struct Queue<T> {
    sender: mpsc::SyncSender<Envelope<T>>,
    state: Arc<State>,
}
pub(super) struct Receiver<T> {
    receiver: mpsc::Receiver<Envelope<T>>,
    state: Arc<State>,
}
struct Envelope<T> {
    value: T,
    _capacity: OwnedSemaphorePermit,
}
/// Before handoff, dropping both permits releases only local, unsent capacity.
pub(super) struct Reserved {
    state: Arc<State>,
    capacity: OwnedSemaphorePermit,
    turn: Turn,
}
/// The actor owns this through terminal AND original thread closure. Waiting
/// caller completion/cancellation is deliberately not represented here.
pub(super) struct Turn {
    _permit: OwnedSemaphorePermit,
    browser: Option<Arc<AtomicBool>>,
    committed: bool,
    settled: bool,
}

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(browser) = self
            .browser
            .as_ref()
            .filter(|_| !self.committed || self.settled)
        {
            browser.store(false, Ordering::Release);
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum TurnMode {
    Text,
    Browser,
}

/// Physical original-process completion alone cannot release a Browser turn.
/// The actor retains this same slot until its exact dual-settlement event.
pub(super) struct Settling {
    pub(super) conversation: String,
    pub(super) reply: u32,
    mode: TurnMode,
    _turn: Turn,
}

impl Settling {
    pub(super) fn new(conversation: String, reply: u32, turn: Turn) -> Self {
        let mode = if turn.browser.is_some() {
            TurnMode::Browser
        } else {
            TurnMode::Text
        };
        Self {
            conversation,
            reply,
            mode,
            _turn: turn,
        }
    }

    pub(super) fn has_local_obligation(&self) -> bool {
        self.mode == TurnMode::Browser
    }

    pub(super) fn matches_browser(&self, conversation: &str, reply: u64) -> bool {
        self.has_local_obligation()
            && self.conversation == conversation
            && u64::from(self.reply) == reply
    }
    /// Only the matching native dual-settlement event consumes this path.
    /// Dropping a committed turn instead leaves queue quarantine sticky.
    pub(super) fn release_browser(mut self) {
        if self.has_local_obligation() {
            self._turn.settled = true;
        }
    }
}

/// Text's existing physical-close fallback is not evidence for Browser tools.
pub(super) fn release_text_after_close(held: &mut Option<Settling>) {
    if held
        .as_ref()
        .is_some_and(|value| !value.has_local_obligation())
    {
        held.take();
    }
}

pub(super) fn channel<T>(capacity: usize) -> (Queue<T>, Receiver<T>) {
    assert!(capacity > 0);
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let state = Arc::new(State {
        gate: Mutex::new(()),
        capacity,
        queue: Arc::new(Semaphore::new(capacity)),
        turn: Arc::new(Semaphore::new(1)),
        browser_turn: Arc::new(AtomicBool::new(false)),
        ready: AtomicBool::new(false),
    });
    (
        Queue {
            sender,
            state: state.clone(),
        },
        Receiver { receiver, state },
    )
}
impl<T> Queue<T> {
    /// Every command, not only turns, consumes the same physical queue budget.
    pub(super) fn enqueue(&self, value: T, revocation: bool) -> Result<(), Error> {
        let _guard = self.state.gate.lock().map_err(|_| Error::Closed)?;
        if !revocation
            && (self.state.turn.available_permits() == 0
                || self.state.browser_turn.load(Ordering::Acquire))
        {
            return Err(Error::Busy);
        }
        let capacity = self
            .state
            .queue
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        self.state.ready.store(false, Ordering::Release);
        self.sender
            .try_send(Envelope {
                value,
                _capacity: capacity,
            })
            .map_err(|_| Error::Closed)
    }
    /// Atomically retire an idle turn lane. Busy leaves all state untouched.
    /// The closure must not call this queue: it runs under the same gate used
    /// by reserve/handoff. This grants no provider or cleanup authority.
    pub(super) fn retire_if_idle<R>(
        &self,
        retire: impl FnOnce() -> Result<R, Error>,
    ) -> Result<R, Error> {
        let _guard = self.state.gate.lock().map_err(|_| Error::Closed)?;
        if self.state.turn.available_permits() == 0
            || self.state.browser_turn.load(Ordering::Acquire)
        {
            return Err(Error::Busy);
        }
        let result = retire()?;
        self.state.ready.store(false, Ordering::Release);
        Ok(result)
    }
    /// A snapshot of original-owner readiness, never evidence of a wire write.
    pub(super) fn set_ready(&self, ready: bool) {
        let Ok(_guard) = self.state.gate.lock() else {
            return;
        };
        self.state.ready.store(ready, Ordering::Release);
    }
    pub(super) fn reserve(&self) -> Result<Reserved, Error> {
        self.reserve_mode(TurnMode::Text)
    }
    pub(super) fn reserve_browser(&self) -> Result<Reserved, Error> {
        self.reserve_mode(TurnMode::Browser)
    }
    fn reserve_mode(&self, mode: TurnMode) -> Result<Reserved, Error> {
        let _guard = self.state.gate.try_lock().map_err(|_| Error::Busy)?;
        if !self.state.ready.load(Ordering::Acquire)
            || self.state.queue.available_permits() != self.state.capacity
            || self.state.browser_turn.load(Ordering::Acquire)
        {
            return Err(Error::Busy);
        }
        let turn = self
            .state
            .turn
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let capacity = self
            .state
            .queue
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let browser = (mode == TurnMode::Browser).then(|| {
            self.state.browser_turn.store(true, Ordering::Release);
            self.state.browser_turn.clone()
        });
        Ok(Reserved {
            state: self.state.clone(),
            capacity,
            turn: Turn {
                _permit: turn,
                browser,
                committed: false,
                settled: false,
            },
        })
    }
    /// Observational lifecycle only. The worker closes ordinary readiness
    /// before checking this under the same gate, preventing an exit from
    /// stranding a pre-say reservation or a synchronous committed handoff.
    pub(super) fn browser_occupied(&self) -> bool {
        let Ok(_guard) = self.state.gate.lock() else {
            return true;
        };
        self.state.browser_turn.load(Ordering::Acquire)
    }
    /// A committed Browser envelope/owner was dropped without the observed
    /// dual receipt. This is quarantine, never a no-send or cleanup proof.
    pub(super) fn browser_handoff_lost(&self) -> bool {
        let _guard = self
            .state
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.state.browser_turn.load(Ordering::Acquire) && self.state.turn.available_permits() != 0
    }
    /// Nonblocking validation suitable for Page's short transcript commit lock.
    pub(super) fn validate(&self, reserved: &Reserved) -> bool {
        let Ok(_guard) = self.state.gate.try_lock() else {
            return false;
        };
        Arc::ptr_eq(&self.state, &reserved.state) && self.state.ready.load(Ordering::Acquire)
    }
    /// One read-only owner inspection for the exact held reservation, before
    /// Page commits a visible reply. It uses this same queue's temporary
    /// capacity; the original handoff capacity and turn remain reserved.
    /// This is not a generic bypass of occupied-turn command admission.
    pub(super) fn inspect_reserved(&self, reserved: &Reserved, value: T) -> Result<(), Error> {
        let _guard = self.state.gate.try_lock().map_err(|_| Error::Busy)?;
        if !Arc::ptr_eq(&self.state, &reserved.state) {
            return Err(Error::Closed);
        }
        if !self.state.ready.load(Ordering::Acquire)
            || self.state.queue.available_permits() != self.state.capacity - 1
        {
            return Err(Error::Busy);
        }
        let capacity = self
            .state
            .queue
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        self.state.ready.store(false, Ordering::Release);
        self.sender
            .try_send(Envelope {
                value,
                _capacity: capacity,
            })
            .map_err(|_| Error::Closed)
    }
    /// Consumes the already-reserved physical slot. Other enqueues cannot take
    /// that slot, so this cannot newly become queue-full after Page.say().
    pub(super) fn handoff(
        &self,
        mut reserved: Reserved,
        build: impl FnOnce(Turn) -> T,
    ) -> Result<(), Error> {
        // The caller has already committed Page.say. Even a poisoned gate or
        // closed receiver is now an unknown handoff, not an unsent reservation.
        reserved.turn.committed = true;
        let _guard = self.state.gate.lock().map_err(|_| Error::Closed)?;
        if !Arc::ptr_eq(&self.state, &reserved.state) {
            return Err(Error::Closed);
        }
        self.state.ready.store(false, Ordering::Release);
        let value = build(reserved.turn);
        self.sender
            .try_send(Envelope {
                value,
                _capacity: reserved.capacity,
            })
            .map_err(|_| Error::Closed)
    }
}
impl<T> Receiver<T> {
    pub(super) fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let _guard = self
            .state
            .gate
            .lock()
            .map_err(|_| mpsc::TryRecvError::Disconnected)?;
        let Envelope { value, _capacity } = self.receiver.try_recv()?;
        self.state.ready.store(false, Ordering::Release);
        drop(_capacity);
        Ok(value)
    }
}
impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.state.ready.store(false, Ordering::Release);
    }
}

/// Native-owner lifecycle, not a renderer-selected or persisted permission.
#[derive(Clone, Copy)]
pub(super) enum OwnerState {
    Active { epoch: u64 },
    Retiring,
    Settled,
}
pub(super) enum SelectionRoute<T, R> {
    Owner(T),
    Committed(R),
}
/// Inspection and the no-owner transaction share the exact registry lock used
/// by owner creation. A retiring/old-epoch process is never treated as absent.
/// The commit is bounded synchronous local storage, never an await or network.
pub(super) fn route_selection<T: Clone, R>(
    owners: &Mutex<std::collections::HashMap<zephium_core::ids::ProfileId, T>>,
    profile: zephium_core::ids::ProfileId,
    expected_epoch: u64,
    current_epoch: impl FnOnce() -> Option<u64>,
    state: impl FnOnce(&T) -> OwnerState,
    commit: impl FnOnce() -> R,
) -> Result<SelectionRoute<T, R>, Error> {
    let owners = owners.lock().map_err(|_| Error::Closed)?;
    if current_epoch() != Some(expected_epoch) {
        return Err(Error::Closed);
    }
    if let Some(owner) = owners.get(&profile) {
        match state(owner) {
            OwnerState::Active { epoch } if epoch == expected_epoch => {
                return Ok(SelectionRoute::Owner(owner.clone()))
            }
            OwnerState::Active { .. } | OwnerState::Retiring => return Err(Error::Busy),
            OwnerState::Settled => {}
        }
    }
    Ok(SelectionRoute::Committed(commit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_inspection_retains_exact_handoff_capacity_and_turn() {
        let (queue, receiver) = channel::<Option<Turn>>(2);
        queue.set_ready(true);
        let reserved = queue.reserve().unwrap();
        queue.inspect_reserved(&reserved, None).unwrap();
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        assert_eq!(queue.enqueue(None, false), Err(Error::Busy));
        assert!(receiver.try_recv().unwrap().is_none());
        // Inspection delivery is not proof that native validation succeeded.
        assert!(!queue.validate(&reserved));
        queue.set_ready(true);
        assert!(queue.validate(&reserved));
        queue.handoff(reserved, Some).unwrap();
        let turn = receiver.try_recv().unwrap().unwrap();
        queue.set_ready(true);
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        drop(turn);
        assert!(queue.reserve().is_ok());
    }

    #[test]
    fn browser_inspection_cannot_borrow_foreign_reservation_or_revoked_lane() {
        let (first, _) = channel::<()>(2);
        let (other, other_receiver) = channel::<()>(2);
        first.set_ready(true);
        other.set_ready(true);
        let reserved = first.reserve().unwrap();
        assert_eq!(other.inspect_reserved(&reserved, ()), Err(Error::Closed));
        assert!(matches!(
            other_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        first.set_ready(false);
        assert_eq!(first.inspect_reserved(&reserved, ()), Err(Error::Busy));
    }

    #[test]
    fn insufficient_inspection_capacity_refuses_before_consuming_handoff() {
        let (queue, receiver) = channel::<Option<Turn>>(1);
        queue.set_ready(true);
        let reserved = queue.reserve().unwrap();
        // There is no second physical queue and no unbudgeted control command.
        // This capacity-one fixture cannot inspect; the original slot survives.
        assert_eq!(queue.inspect_reserved(&reserved, None), Err(Error::Busy));
        assert!(queue.validate(&reserved));
        queue.handoff(reserved, Some).unwrap();
        drop(receiver.try_recv().unwrap());
    }

    #[test]
    fn browser_slot_survives_worker_exit_unwind_and_lost_local_receiver() {
        let (queue, receiver) = channel::<Settling>(2);
        queue.set_ready(true);
        let reserved = queue.reserve_browser().unwrap();
        assert!(queue.browser_occupied());
        queue
            .handoff(reserved, |turn| {
                Settling::new("same-conversation".into(), 7, turn)
            })
            .unwrap();
        let mut held = Some(receiver.try_recv().unwrap());
        // The actual worker's exit and catch_unwind paths use this helper.
        // Neither physical process closure nor a lost Page receipt is enough.
        release_text_after_close(&mut held);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            release_text_after_close(&mut held);
            panic!("synthetic worker unwind");
        }));
        assert!(unwind.is_err());
        assert!(held.as_ref().unwrap().has_local_obligation());
        queue.set_ready(true);
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        assert_eq!(queue.retire_if_idle(|| Ok(())), Err(Error::Busy));
        assert!(!held
            .as_ref()
            .unwrap()
            .matches_browser("other-conversation", 7));
        assert!(!held
            .as_ref()
            .unwrap()
            .matches_browser("same-conversation", 8));
        assert!(held
            .as_ref()
            .unwrap()
            .matches_browser("same-conversation", 7));
        // Only the native owner's matching BrowserSettled event takes this
        // value in production. The test supplies that exact local projection.
        held.take().unwrap().release_browser();
        assert!(!queue.browser_occupied());
        assert!(queue.reserve().is_ok());
    }

    #[test]
    fn stopped_worker_must_wait_for_browser_reservation_drop_or_synchronous_handoff() {
        let (queue, receiver) = channel::<Turn>(2);
        queue.set_ready(true);
        let before_say = queue.reserve_browser().unwrap();
        queue.set_ready(false); // ordinary admission retired under same gate
        assert!(queue.browser_occupied());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(before_say); // no Page/native dispatch obligation was created
        assert!(!queue.browser_occupied());

        queue.set_ready(true);
        let committed = queue.reserve_browser().unwrap();
        queue.set_ready(false);
        // Post-say commit cannot be turned into a newly full/refused queue.
        queue.handoff(committed, |turn| turn).unwrap();
        assert!(queue.browser_occupied());
        let actor_owned = receiver.try_recv().unwrap();
        assert!(queue.browser_occupied());
        Settling::new("same-conversation".into(), 1, actor_owned).release_browser();
        assert!(!queue.browser_occupied());
    }

    #[test]
    fn queued_committed_browser_drop_cannot_masquerade_as_unsent_reservation_release() {
        let (queue, receiver) = channel::<Turn>(2);
        queue.set_ready(true);
        let reserved = queue.reserve_browser().unwrap();
        queue.handoff(reserved, |turn| turn).unwrap();
        // A panic/worker exit before adoption drops the queued envelope. No
        // actual Page receipt was consumed, even though the semaphore frees.
        drop(receiver);
        assert!(queue.browser_occupied());
        assert!(queue.browser_handoff_lost());
        queue.set_ready(true);
        assert!(matches!(queue.reserve_browser(), Err(Error::Busy)));
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        assert_eq!(queue.retire_if_idle(|| Ok(())), Err(Error::Busy));
    }

    #[test]
    fn panic_during_post_say_handoff_keeps_browser_quarantine_sticky() {
        let (queue, _receiver) = channel::<Turn>(2);
        queue.set_ready(true);
        let reserved = queue.reserve_browser().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = queue.handoff(reserved, |_turn| panic!("synthetic handoff unwind"));
        }));
        assert!(result.is_err());
        assert!(queue.browser_occupied());
        assert!(queue.browser_handoff_lost());
        assert!(queue.reserve().is_err());
    }

    #[test]
    fn gate_failure_after_page_commit_cannot_release_as_unsent() {
        let (queue, _receiver) = channel::<Turn>(2);
        queue.set_ready(true);
        let reserved = queue.reserve_browser().unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = queue.state.gate.lock().unwrap();
            panic!("synthetic failure after Page validation");
        }));
        assert_eq!(queue.handoff(reserved, |turn| turn), Err(Error::Closed));
        assert!(queue.browser_handoff_lost());
        assert!(queue.reserve().is_err());
    }

    #[test]
    fn text_close_fallback_never_accepts_a_browser_settlement_event() {
        let (queue, receiver) = channel::<Settling>(1);
        queue.set_ready(true);
        let reserved = queue.reserve().unwrap();
        queue
            .handoff(reserved, |turn| {
                Settling::new("text-conversation".into(), 3, turn)
            })
            .unwrap();
        let mut held = Some(receiver.try_recv().unwrap());
        assert!(!held
            .as_ref()
            .unwrap()
            .matches_browser("text-conversation", 3));
        release_text_after_close(&mut held);
        assert!(held.is_none());
        queue.set_ready(true);
        assert!(queue.reserve().is_ok());
    }
    #[test]
    fn lifecycle_busy_is_atomic_and_side_effect_free_until_actual_turn_release() {
        let (queue, receiver) = channel::<Turn>(2);
        queue.set_ready(true);
        let reserved = queue.reserve().unwrap();
        let revision = AtomicBool::new(false);
        assert_eq!(
            queue.retire_if_idle(|| {
                revision.store(true, Ordering::Release);
                Ok(())
            }),
            Err(Error::Busy)
        );
        assert!(!revision.load(Ordering::Acquire));
        assert!(queue.validate(&reserved)); // Busy did not change readiness
        queue.handoff(reserved, |turn| turn).unwrap();
        assert_eq!(
            queue.retire_if_idle(|| {
                revision.store(true, Ordering::Release);
                Ok(())
            }),
            Err(Error::Busy)
        );
        let active = receiver.try_recv().unwrap();
        assert_eq!(
            queue.retire_if_idle(|| {
                revision.store(true, Ordering::Release);
                Ok(())
            }),
            Err(Error::Busy)
        );
        assert!(!revision.load(Ordering::Acquire));
        // The actual caller owns this through original terminal+Closed; neither
        // receiving the queue command nor returning a UI result releases it.
        drop(active);
        queue.set_ready(true);
        queue
            .retire_if_idle(|| {
                assert!(matches!(queue.reserve(), Err(Error::Busy))); // same gate, no check/retire gap
                revision.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap();
        assert!(revision.load(Ordering::Acquire));
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
    }
    #[test]
    fn full_queue_and_in_progress_command_cannot_reserve() {
        let (queue, receiver) = channel(1);
        queue.set_ready(true);
        queue.enqueue(1, false).unwrap();
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        assert_eq!(receiver.try_recv().unwrap(), 1);
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        queue.set_ready(true);
        assert!(queue.reserve().is_ok());
    }
    #[test]
    fn dropped_unsent_reservation_releases_both_slots() {
        let (queue, _receiver) = channel::<()>(1);
        queue.set_ready(true);
        let first = queue.reserve().unwrap();
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        assert_eq!(queue.enqueue((), false), Err(Error::Busy));
        drop(first);
        assert!(queue.reserve().is_ok());
    }
    #[test]
    fn revocation_cannot_steal_reserved_queue_capacity() {
        let (queue, receiver) = channel(2);
        queue.set_ready(true);
        let reserved = queue.reserve().unwrap();
        queue.enqueue((1, None), true).unwrap();
        assert_eq!(queue.enqueue((3, None), true), Err(Error::Busy));
        queue.handoff(reserved, |turn| (2, Some(turn))).unwrap();
        assert_eq!(receiver.try_recv().unwrap().0, 1);
        let (_, turn) = receiver.try_recv().unwrap();
        queue.set_ready(true);
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        drop(turn);
        assert!(queue.reserve().is_ok());
    }
    #[test]
    fn consumed_reply_slot_remains_owned_after_waiter_cancel_until_actor_closes() {
        let (queue, receiver) = channel(1);
        queue.set_ready(true);
        let reserved = queue.reserve().unwrap();
        let (result, waiter) = tokio::sync::oneshot::channel::<()>();
        queue.handoff(reserved, |turn| (turn, result)).unwrap();
        drop(waiter);
        let (actor_turn, result) = receiver.try_recv().unwrap();
        assert!(result.send(()).is_err());
        queue.set_ready(true); // A terminal alone is not physical/semantic closure.
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        drop(actor_turn); // Exact Closed or completed physical teardown only.
        assert!(queue.reserve().is_ok());
    }
    #[test]
    fn separate_owners_do_not_share_slots_and_validation_is_owner_bound() {
        let (first, _r1) = channel::<()>(1);
        let (second, _r2) = channel::<()>(1);
        first.set_ready(true);
        second.set_ready(true);
        let reserved = first.reserve().unwrap();
        assert!(!second.validate(&reserved));
        assert!(second.reserve().is_ok());
        first.set_ready(false);
        assert!(!first.validate(&reserved));
    }
    #[test]
    fn simultaneous_reservers_admit_exactly_one() {
        let (queue, _receiver) = channel::<()>(1);
        let queue = Arc::new(queue);
        queue.set_ready(true);
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let threads = (0..8)
            .map(|_| {
                let queue = queue.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    queue.reserve().ok()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let held = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(held.len(), 1);
    }
    #[test]
    fn old_epoch_or_retiring_owner_never_qualifies_for_direct_selection() {
        let profile = zephium_core::ids::ProfileId::from(1_u128);
        for state in [OwnerState::Active { epoch: 1 }, OwnerState::Retiring] {
            let owners = Mutex::new(std::collections::HashMap::from([(profile, state)]));
            let mut commits = 0;
            let result = route_selection(
                &owners,
                profile,
                3,
                || Some(3),
                |state| *state,
                || {
                    commits += 1;
                },
            );
            assert!(matches!(result, Err(Error::Busy)));
            assert_eq!(commits, 0);
        }
    }
    #[test]
    fn no_owner_commit_excludes_concurrent_creation_until_local_receipt() {
        let profile = zephium_core::ids::ProfileId::from(1_u128);
        let owners = Arc::new(Mutex::new(std::collections::HashMap::<_, OwnerState>::new()));
        let captured = owners.clone();
        let result = route_selection(
            &owners,
            profile,
            2,
            || Some(2),
            |state| *state,
            || {
                std::thread::spawn(move || {
                    assert!(matches!(
                        captured.try_lock(),
                        Err(std::sync::TryLockError::WouldBlock)
                    ))
                })
                .join()
                .unwrap();
                19
            },
        );
        assert!(matches!(result, Ok(SelectionRoute::Committed(19))));
        owners
            .lock()
            .unwrap()
            .insert(profile, OwnerState::Active { epoch: 2 });
        assert!(matches!(
            route_selection(
                &owners,
                profile,
                2,
                || Some(2),
                |state| *state,
                || panic!("live owner bypass")
            ),
            Ok(SelectionRoute::Owner(_))
        ));
    }
    #[test]
    fn selection_queue_admission_blocks_page_until_confirmed_readiness_even_after_receive() {
        let (queue, receiver) = channel::<&str>(2);
        queue.set_ready(true);
        queue.enqueue("selection", false).unwrap();
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        assert_eq!(receiver.try_recv().unwrap(), "selection");
        // Store request is in progress; cached old Connected presentation is
        // not readiness. A failed/uncertain acknowledgement keeps this closed.
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        queue.set_ready(false);
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        // Only a confirmed commit/readback plus original-owner readiness may reopen.
        queue.set_ready(true);
        let turn = queue.reserve().unwrap();
        assert_eq!(queue.enqueue("another selection", false), Err(Error::Busy));
        drop(turn);
    }
}
