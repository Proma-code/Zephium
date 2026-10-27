//! Local queue capacity and move-only turn ownership. No provider authority.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex, MutexGuard, Weak,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Busy,
    Closed,
}
/// Cancellation-only joins for the same caller bits retained by native writers.
/// Registration and permanent retirement share one lock; neither grants work.
#[derive(Default)]
pub(super) struct Cancellations(Mutex<(bool, Vec<Weak<AtomicBool>>)>);
impl Cancellations {
    pub(super) fn register(&self, cancel: &Arc<AtomicBool>) -> Result<(), Error> {
        let mut state = self.0.lock().map_err(|_| Error::Closed)?;
        if state.0 {
            cancel.store(true, Ordering::Release);
            return Err(Error::Closed);
        }
        state.1.retain(|value| value.strong_count() != 0);
        if state
            .1
            .iter()
            .any(|value| value.ptr_eq(&Arc::downgrade(cancel)))
        {
            return Ok(());
        }
        if state.1.len() >= 64 {
            return Err(Error::Busy);
        }
        state.1.push(Arc::downgrade(cancel));
        Ok(())
    }
    pub(super) fn retire(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.0 = true;
        for cancel in state.1.iter().filter_map(Weak::upgrade) {
            cancel.store(true, Ordering::Release);
        }
        state.1.clear();
    }
}

/// Local, move-only transfer. Until committed, early return or unwind restores
/// the exact value under the original lock. Never hold this across native I/O.
pub(super) struct Transfer<'a, T> {
    slot: MutexGuard<'a, Option<T>>,
    value: Option<T>,
}
impl<'a, T> Transfer<'a, T> {
    pub(super) fn take(slot: &'a Mutex<Option<T>>) -> Result<Option<Self>, Error> {
        let mut slot = slot.lock().map_err(|_| Error::Closed)?;
        let Some(value) = slot.take() else {
            return Ok(None);
        };
        Ok(Some(Self {
            slot,
            value: Some(value),
        }))
    }
    pub(super) fn value(&self) -> &T {
        self.value.as_ref().expect("uncommitted local transfer")
    }
    pub(super) fn commit(mut self) -> T {
        self.value.take().expect("uncommitted local transfer")
    }
}
impl<T> Drop for Transfer<'_, T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            *self.slot = Some(value);
        }
    }
}

struct State {
    gate: Mutex<()>,
    capacity: usize,
    queue: Arc<Semaphore>,
    turn: Arc<Semaphore>,
    browser_turn: Arc<AtomicBool>,
    ready: AtomicBool,
    parked: AtomicBool,
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
/// An explicit, unsent Reply owns the same physical lane while a newly opened
/// runtime is preparing. It cannot be handed off until current readiness is
/// observed and this exact value is refined into Reserved.
pub(super) struct Demand(Reserved);

/// The same queue gate is sealed while its owner attempts healthy retirement.
/// Dropping this value stays closed. Only a returned, unchanged native owner
/// may explicitly reopen it after a NotReady result.
pub(super) struct Parking(Arc<State>);
impl Parking {
    pub(super) fn reopen(self) {
        let Ok(_guard) = self.0.gate.lock() else {
            return;
        };
        self.0.parked.store(false, Ordering::Release);
    }
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
        parked: AtomicBool::new(false),
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
        if self.state.parked.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
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
            || self.state.parked.load(Ordering::Acquire)
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
        self.state.ready.store(
            ready && !self.state.parked.load(Ordering::Acquire),
            Ordering::Release,
        );
    }
    pub(super) fn reserve(&self) -> Result<Reserved, Error> {
        self.reserve_mode(TurnMode::Text)
    }
    pub(super) fn reserve_browser(&self) -> Result<Reserved, Error> {
        self.reserve_mode(TurnMode::Browser)
    }
    fn reserve_mode(&self, mode: TurnMode) -> Result<Reserved, Error> {
        self.reserve_local(mode, true)
    }
    pub(super) fn demand_browser(&self) -> Result<Demand, Error> {
        self.reserve_local(TurnMode::Browser, false).map(Demand)
    }
    pub(super) fn demand_command(&self) -> Result<Demand, Error> {
        self.reserve_local(TurnMode::Text, false).map(Demand)
    }
    /// Consume an explicit non-model wake demand into this SAME queue without
    /// exposing a drop-then-enqueue gap to parking or another caller. It confers
    /// no native action authority; the receiving owner still checks the command.
    pub(super) fn handoff_command(&self, demand: Demand, value: T) -> Result<(), Error> {
        let _guard = self.state.gate.lock().map_err(|_| Error::Closed)?;
        if !Arc::ptr_eq(&self.state, &demand.0.state)
            || self.state.parked.load(Ordering::Acquire)
            || demand.0.turn.browser.is_some()
        {
            return Err(Error::Closed);
        }
        let Demand(Reserved { capacity, turn, .. }) = demand;
        self.state.ready.store(false, Ordering::Release);
        let result = self
            .sender
            .try_send(Envelope {
                value,
                _capacity: capacity,
            })
            .map_err(|_| Error::Closed);
        drop(turn);
        result
    }
    fn reserve_local(&self, mode: TurnMode, require_ready: bool) -> Result<Reserved, Error> {
        let _guard = self.state.gate.try_lock().map_err(|_| Error::Busy)?;
        if (require_ready && !self.state.ready.load(Ordering::Acquire))
            || self.state.parked.load(Ordering::Acquire)
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
    pub(super) fn refine_demand(&self, demand: Demand) -> Result<Reserved, (Error, Demand)> {
        let Ok(_guard) = self.state.gate.try_lock() else {
            return Err((Error::Busy, demand));
        };
        if !Arc::ptr_eq(&self.state, &demand.0.state) || self.state.parked.load(Ordering::Acquire) {
            return Err((Error::Closed, demand));
        }
        if !self.state.ready.load(Ordering::Acquire)
            || self.state.queue.available_permits() != self.state.capacity - 1
        {
            return Err((Error::Busy, demand));
        }
        Ok(demand.0)
    }
    /// No queued command or reserved capacity can cross this transition.
    /// `quiescent` is a nonblocking local ownership check; no I/O/queue call.
    pub(super) fn try_park(&self, quiescent: impl FnOnce() -> bool) -> Result<Parking, Error> {
        let _guard = self.state.gate.try_lock().map_err(|_| Error::Busy)?;
        if self.state.parked.load(Ordering::Acquire)
            || self.state.queue.available_permits() != self.state.capacity
            || self.state.turn.available_permits() != 1
            || self.state.browser_turn.load(Ordering::Acquire)
            || !quiescent()
        {
            return Err(Error::Busy);
        }
        self.state.parked.store(true, Ordering::Release);
        self.state.ready.store(false, Ordering::Release);
        Ok(Parking(self.state.clone()))
    }
    pub(super) fn is_parked(&self) -> bool {
        self.state.parked.load(Ordering::Acquire)
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
        Arc::ptr_eq(&self.state, &reserved.state)
            && !self.state.parked.load(Ordering::Acquire)
            && self.state.ready.load(Ordering::Acquire)
    }
    /// One read-only owner inspection for the exact held reservation, before
    /// Page commits a visible reply. It uses this same queue's temporary
    /// capacity; the original handoff capacity and turn remain reserved.
    /// This is not a generic bypass of occupied-turn command admission.
    pub(super) fn inspect_reserved(&self, reserved: &Reserved, value: T) -> Result<(), Error> {
        let _guard = self.state.gate.try_lock().map_err(|_| Error::Busy)?;
        if !Arc::ptr_eq(&self.state, &reserved.state) || self.state.parked.load(Ordering::Acquire) {
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
        if !Arc::ptr_eq(&self.state, &reserved.state) || self.state.parked.load(Ordering::Acquire) {
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

    #[test]
    fn local_transfer_restores_exact_custody_until_committed_once() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        struct Token(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Token {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }
        let slot = Mutex::new(Some(Token(drops.clone())));
        {
            let transfer = Transfer::take(&slot).unwrap().unwrap();
            assert!(Arc::ptr_eq(&transfer.value().0, &drops));
            assert!(matches!(
                slot.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            // Local validation failure/early return restores, without cloning.
        }
        assert_eq!(drops.load(Ordering::Acquire), 0);
        let token = Transfer::take(&slot).unwrap().unwrap().commit();
        assert!(Transfer::take(&slot).unwrap().is_none());
        assert_eq!(drops.load(Ordering::Acquire), 0);
        drop(token);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
    #[test]
    fn local_transfer_unwind_restores_value_without_claiming_native_settlement() {
        let token = Arc::new(());
        let slot = Mutex::new(Some(token.clone()));
        assert!(std::panic::catch_unwind(|| {
            let _transfer = Transfer::take(&slot).unwrap().unwrap();
            panic!("local preparation failed");
        })
        .is_err());
        let restored = slot.lock().unwrap_or_else(|error| error.into_inner());
        assert!(Arc::ptr_eq(restored.as_ref().unwrap(), &token));
    }

    #[test]
    fn action_cancellation_registration_and_retirement_are_one_permanent_join() {
        let cancellations = Arc::new(Cancellations::default());
        let first = Arc::new(AtomicBool::new(false));
        cancellations.register(&first).unwrap();
        cancellations.register(&first).unwrap();
        let second = Arc::new(AtomicBool::new(false));
        let owned = cancellations.clone();
        let token = second.clone();
        let registering = std::thread::spawn(move || owned.register(&token));
        cancellations.retire();
        let _ = registering.join().unwrap();
        assert!(first.load(Ordering::Acquire));
        assert!(second.load(Ordering::Acquire));
        let late = Arc::new(AtomicBool::new(false));
        assert_eq!(cancellations.register(&late), Err(Error::Closed));
        assert!(late.load(Ordering::Acquire));
    }
    #[test]
    fn action_cancellation_registry_is_bounded_and_does_not_keep_actions_alive() {
        let cancellations = Cancellations::default();
        let tokens: Vec<_> = (0..64).map(|_| Arc::new(AtomicBool::new(false))).collect();
        for token in &tokens {
            cancellations.register(token).unwrap();
        }
        assert_eq!(
            cancellations.register(&Arc::new(AtomicBool::new(false))),
            Err(Error::Busy)
        );
        let weak = Arc::downgrade(&tokens[0]);
        drop(tokens);
        assert!(weak.upgrade().is_none());
        assert!(cancellations
            .register(&Arc::new(AtomicBool::new(false)))
            .is_ok());
    }

    #[test]
    fn command_wake_handoff_has_no_unowned_gap_for_parking_or_a_turn() {
        let (queue, receiver) = channel::<&str>(2);
        let demand = queue.demand_command().unwrap();
        assert!(matches!(queue.try_park(|| true), Err(Error::Busy)));
        assert_eq!(queue.enqueue("other", false), Err(Error::Busy));
        queue.handoff_command(demand, "quota").unwrap();
        assert!(matches!(queue.try_park(|| true), Err(Error::Busy)));
        assert!(matches!(queue.demand_browser(), Err(Error::Busy)));
        assert_eq!(receiver.try_recv().unwrap(), "quota");
        // Receiving is not native settlement: the owner's independent predicate
        // remains false until the actual read has settled.
        assert!(matches!(queue.try_park(|| false), Err(Error::Busy)));
        assert!(queue.try_park(|| true).is_ok());
    }
    #[test]
    fn browser_demand_cannot_be_consumed_as_an_observation_command() {
        let (queue, receiver) = channel::<u8>(2);
        let demand = queue.demand_browser().unwrap();
        assert_eq!(queue.handoff_command(demand, 1), Err(Error::Closed));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!queue.browser_occupied());
        assert!(queue.try_park(|| true).is_ok());
    }
    #[test]
    fn command_wake_cannot_cross_queue_identity_or_reopen_retired_queue() {
        let (old, _old_receiver) = channel::<u8>(2);
        let (fresh, receiver) = channel::<u8>(2);
        let demand = old.demand_command().unwrap();
        assert_eq!(fresh.handoff_command(demand, 1), Err(Error::Closed));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(old.try_park(|| true).unwrap());
        fresh.set_ready(true);
        assert!(matches!(old.demand_command(), Err(Error::Busy)));
        assert!(fresh.demand_command().is_ok());
    }

    #[test]
    fn parking_requires_all_command_capacity_not_only_an_idle_turn() {
        let (queue, receiver) = channel::<u8>(2);
        queue.set_ready(true);
        queue.enqueue(1, false).unwrap();
        assert!(matches!(queue.try_park(|| true), Err(Error::Busy)));
        assert!(!queue.is_parked());
        assert_eq!(receiver.try_recv().unwrap(), 1);
        // The actor must independently prove the received operation settled.
        assert!(matches!(queue.try_park(|| false), Err(Error::Busy)));
        let parked = queue.try_park(|| true).unwrap();
        assert!(queue.is_parked());
        assert_eq!(queue.enqueue(2, false), Err(Error::Closed));
        assert_eq!(queue.enqueue(3, true), Err(Error::Closed));
        queue.set_ready(true);
        assert!(matches!(queue.reserve_browser(), Err(Error::Busy)));
        assert!(matches!(queue.demand_browser(), Err(Error::Busy)));
        assert_eq!(
            queue.retire_if_idle(|| panic!("retirement while parked")),
            Err::<(), _>(Error::Busy)
        );
        parked.reopen();
        assert!(!queue.is_parked());
        assert!(matches!(queue.reserve(), Err(Error::Busy)));
        queue.set_ready(true);
        assert!(queue.reserve().is_ok());
    }

    #[test]
    fn unsent_wake_demand_owns_same_turn_and_capacity_until_current_refinement() {
        let (queue, _receiver) = channel::<()>(2);
        let demand = queue.demand_browser().unwrap();
        assert!(matches!(queue.try_park(|| true), Err(Error::Busy)));
        assert_eq!(queue.enqueue((), false), Err(Error::Busy));
        let (error, demand) = match queue.refine_demand(demand) {
            Err(value) => value,
            Ok(_) => panic!("unready demand became reservation"),
        };
        assert_eq!(error, Error::Busy);
        queue.set_ready(true);
        let reserved = queue.refine_demand(demand).unwrap_or_else(|_| panic!());
        assert!(queue.validate(&reserved));
        assert!(matches!(queue.try_park(|| true), Err(Error::Busy)));
        drop(reserved);
        assert!(queue.try_park(|| true).is_ok());
    }

    #[test]
    fn dropped_unsent_demand_releases_but_committed_browser_loss_still_blocks_parking() {
        let (queue, receiver) = channel::<Turn>(2);
        drop(queue.demand_browser().unwrap());
        let parked = queue.try_park(|| true).unwrap();
        parked.reopen();
        queue.set_ready(true);
        let demand = queue.demand_browser().unwrap();
        let reserved = queue.refine_demand(demand).unwrap_or_else(|_| panic!());
        queue.handoff(reserved, |turn| turn).unwrap();
        assert!(matches!(queue.try_park(|| true), Err(Error::Busy)));
        drop(receiver.try_recv().unwrap());
        assert!(queue.browser_handoff_lost());
        assert!(matches!(queue.try_park(|| true), Err(Error::Busy)));
    }

    #[test]
    fn foreign_demand_never_refines_or_releases_the_held_owner_lane() {
        let (first, _first_receiver) = channel::<()>(2);
        let (second, _second_receiver) = channel::<()>(2);
        first.set_ready(true);
        second.set_ready(true);
        let demand = first.demand_browser().unwrap();
        let (error, demand) = match second.refine_demand(demand) {
            Err(value) => value,
            Ok(_) => panic!("foreign demand accepted"),
        };
        assert_eq!(error, Error::Closed);
        assert!(matches!(first.try_park(|| true), Err(Error::Busy)));
        drop(demand);
        assert!(first.try_park(|| true).is_ok());
        assert!(second.try_park(|| true).is_ok());
    }

    #[test]
    fn dropped_parking_guard_never_reopens_after_unknown_retirement() {
        let (queue, _receiver) = channel::<()>(2);
        drop(queue.try_park(|| true).unwrap());
        queue.set_ready(true);
        assert!(queue.is_parked());
        assert_eq!(queue.enqueue((), true), Err(Error::Closed));
        assert!(matches!(queue.demand_browser(), Err(Error::Busy)));
    }
}
