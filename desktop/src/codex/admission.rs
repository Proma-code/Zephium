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
}

pub(super) fn channel<T>(capacity: usize) -> (Queue<T>, Receiver<T>) {
    assert!(capacity > 0);
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let state = Arc::new(State {
        gate: Mutex::new(()),
        capacity,
        queue: Arc::new(Semaphore::new(capacity)),
        turn: Arc::new(Semaphore::new(1)),
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
        if !revocation && self.state.turn.available_permits() == 0 {
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
        if self.state.turn.available_permits() == 0 {
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
        let _guard = self.state.gate.try_lock().map_err(|_| Error::Busy)?;
        if !self.state.ready.load(Ordering::Acquire)
            || self.state.queue.available_permits() != self.state.capacity
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
        Ok(Reserved {
            state: self.state.clone(),
            capacity,
            turn: Turn { _permit: turn },
        })
    }
    /// Nonblocking validation suitable for Page's short transcript commit lock.
    pub(super) fn validate(&self, reserved: &Reserved) -> bool {
        let Ok(_guard) = self.state.gate.try_lock() else {
            return false;
        };
        Arc::ptr_eq(&self.state, &reserved.state) && self.state.ready.load(Ordering::Acquire)
    }
    /// Consumes the already-reserved physical slot. Other enqueues cannot take
    /// that slot, so this cannot newly become queue-full after Page.say().
    pub(super) fn handoff(
        &self,
        reserved: Reserved,
        build: impl FnOnce(Turn) -> T,
    ) -> Result<(), Error> {
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
