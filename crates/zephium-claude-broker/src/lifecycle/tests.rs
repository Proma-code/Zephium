use super::*;
use crate::{custody::Owner, protocol::AuthStatus};
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{atomic::AtomicUsize, mpsc},
    thread,
};

fn profile() -> Profile {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 55);
    u128::from(NEXT.fetch_add(1, Ordering::Relaxed)).to_be_bytes()
}
fn selected() -> (TrialAccounts, NativeScope) {
    let accounts = TrialAccounts::new();
    let profile = profile();
    accounts.profiles_changed(&[profile], Some(profile));
    let scope = accounts.current_scope().unwrap();
    (accounts, scope)
}
fn owner(scope: NativeScope) -> AccountOwner {
    AccountOwner::new(Owner {
        profile: u128::from_be_bytes(scope.profile),
        account_generation: 1,
        profile_generation: scope.epoch,
    })
    .unwrap()
}
struct TestFactory<F>(F);
impl<F: FnMut(&PreparationControl) -> Result<Resource, Error>> Factory for TestFactory<F> {
    fn prepare(&mut self, control: &PreparationControl) -> Result<Resource, Error> {
        (self.0)(control)
    }
}
struct TestPort<F> {
    run: F,
    drops: Arc<AtomicUsize>,
}
impl<F> Drop for TestPort<F> {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl<F: FnMut(&account::Control) -> Result<Reply, account::Error>> Port for TestPort<F> {
    fn run(&mut self, _: Request, control: &account::Control) -> Result<Reply, account::Error> {
        (self.run)(control)
    }
}
fn resource(scope: NativeScope, drops: Arc<AtomicUsize>) -> Resource {
    Resource::new(
        scope,
        owner(scope),
        TestPort {
            run: |_: &account::Control| Ok(Reply::Status(AuthStatus::SignedOut)),
            drops,
        },
    )
    .unwrap()
}
fn install(accounts: &TrialAccounts, scope: NativeScope, drops: Arc<AtomicUsize>) {
    accounts
        .prepare(scope)
        .unwrap()
        .run(&mut TestFactory(|control: &PreparationControl| {
            assert!(!control.cancelled(Instant::now()));
            Ok(resource(control.scope(), Arc::clone(&drops)))
        }))
        .unwrap();
}

#[test]
fn public_closed_facade_never_admits_registration_or_invents_authentication() {
    let (accounts, scope) = selected();
    assert!(crate::availability().is_err());
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::NoStoreUnderClosedGate
    );
    accounts.profiles_changed(&[scope.profile], Some(scope.profile));
    assert_eq!(accounts.current_scope(), Err(Error::Stale));
    accounts.close();
    assert!(accounts.closed_and_drained());
}
#[test]
fn native_epochs_and_registry_instances_reject_away_back_and_foreign_scopes() {
    let (accounts, a) = selected();
    let b = profile();
    accounts.profiles_changed(&[a.profile, b], Some(b));
    accounts.profiles_changed(&[a.profile, b], Some(a.profile));
    let current = accounts.current_scope().unwrap();
    assert_ne!(a, current);
    assert!(matches!(accounts.prepare(a), Err(Error::Stale)));
    let other = TrialAccounts::new();
    other.profiles_changed(&[a.profile], Some(a.profile));
    assert!(matches!(other.prepare(current), Err(Error::Stale)));
}
#[test]
fn queued_preparation_is_registered_before_factory_and_drop_proves_never_entered() {
    let (accounts, scope) = selected();
    let preparation = accounts.prepare(scope).unwrap();
    assert!(matches!(accounts.prepare(scope), Err(Error::Busy)));
    assert_eq!(accounts.retire_profile(scope.profile), Retirement::Pending);
    accounts.close();
    assert!(!accounts.closed_and_drained());
    drop(preparation);
    assert!(accounts.closed_and_drained());
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::StorageUnconfirmed
    );
}
#[test]
fn running_preparation_close_waits_for_actual_return_and_retires_late_resource() {
    let (accounts, scope) = selected();
    let preparation = accounts.prepare(scope).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let returned_drops = Arc::clone(&drops);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        preparation.run(&mut TestFactory(|control: &PreparationControl| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(control.cancelled(Instant::now()));
            Ok(resource(control.scope(), Arc::clone(&returned_drops)))
        }))
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    accounts.close();
    assert!(!accounts.closed_and_drained());
    assert_eq!(accounts.retire_profile(scope.profile), Retirement::Pending);
    release_tx.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Err(Error::Closed));
    assert!(accounts.closed_and_drained());
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::StorageUnconfirmed
    );
    let state = accounts.inner.state();
    assert!(
        state.entries[&scope.profile]
            .resource
            .as_ref()
            .unwrap()
            .owner
            .view()
            .closed
    );
    drop(state);
    drop(accounts);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn dropped_facade_keeps_preparation_and_its_late_resource_until_worker_returns() {
    let (accounts, scope) = selected();
    let preparation = accounts.prepare(scope).unwrap();
    let weak = Arc::downgrade(&accounts.inner);
    let drops = Arc::new(AtomicUsize::new(0));
    let returned_drops = Arc::clone(&drops);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        preparation.run(&mut TestFactory(|control: &PreparationControl| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(control.cancelled(Instant::now()));
            Ok(resource(control.scope(), Arc::clone(&returned_drops)))
        }))
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(accounts);
    assert!(weak.upgrade().is_some());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    release_tx.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Err(Error::Closed));
    assert!(weak.upgrade().is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn preparation_unwinding_is_not_completion_and_retains_pending_custody() {
    let (accounts, scope) = selected();
    let preparation = accounts.prepare(scope).unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _ = preparation.run(&mut TestFactory(
            |_: &PreparationControl| -> Result<Resource, Error> {
                panic!("synthetic preparation unwind")
            },
        ));
    }))
    .is_err());
    accounts.close();
    assert!(!accounts.closed_and_drained());
    assert_eq!(accounts.retire_profile(scope.profile), Retirement::Pending);
    assert!(
        accounts.inner.state().entries[&scope.profile]
            .preparation
            .as_ref()
            .unwrap()
            .uncertain
    );
}
#[test]
fn preparation_returned_error_discharge_does_not_claim_persistent_erasure() {
    let (accounts, scope) = selected();
    let result =
        accounts
            .prepare(scope)
            .unwrap()
            .run(&mut TestFactory(|_: &PreparationControl| {
                Err(Error::Preparation)
            }));
    assert_eq!(result, Err(Error::Preparation));
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::StorageUnconfirmed
    );
    accounts.close();
    assert!(accounts.closed_and_drained());
}
#[test]
fn actual_account_work_close_retains_port_and_waits_for_settled_return() {
    let (accounts, scope) = selected();
    let drops = Arc::new(AtomicUsize::new(0));
    let resource_drops = Arc::clone(&drops);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut channels = Some((entered_tx, release_rx));
    accounts
        .prepare(scope)
        .unwrap()
        .run(&mut TestFactory(|control: &PreparationControl| {
            let (entered_tx, release_rx) = channels.take().unwrap();
            Resource::new(
                control.scope(),
                owner(control.scope()),
                TestPort {
                    drops: Arc::clone(&resource_drops),
                    run: move |control: &account::Control| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        assert!(control.cancelled(Instant::now()));
                        Ok(Reply::Status(AuthStatus::SignedOut))
                    },
                },
            )
        }))
        .unwrap();
    let work = accounts
        .begin(scope, Request::ReadStatus, Instant::now())
        .unwrap();
    let worker = thread::spawn(move || work.run());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    accounts.close();
    assert_eq!(accounts.retire_profile(scope.profile), Retirement::Pending);
    assert!(!accounts.closed_and_drained());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    release_tx.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Err(account::Error::Closed));
    assert!(accounts.closed_and_drained());
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::StorageUnconfirmed
    );
    drop(accounts);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn actual_account_work_unwind_retains_the_same_port_and_blocks_drain() {
    let (accounts, scope) = selected();
    let drops = Arc::new(AtomicUsize::new(0));
    accounts
        .prepare(scope)
        .unwrap()
        .run(&mut TestFactory(|control: &PreparationControl| {
            Resource::new(
                control.scope(),
                owner(control.scope()),
                TestPort {
                    drops: Arc::clone(&drops),
                    run: |_: &account::Control| -> Result<Reply, account::Error> {
                        panic!("synthetic port unwind")
                    },
                },
            )
        }))
        .unwrap();
    let work = accounts
        .begin(scope, Request::ReadStatus, Instant::now())
        .unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| work.run())).is_err());
    accounts.close();
    assert!(!accounts.closed_and_drained());
    assert_eq!(accounts.retire_profile(scope.profile), Retirement::Pending);
    drop(accounts);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
}
#[test]
fn queued_work_drop_uses_actual_owner_never_started_release_without_clean_claim() {
    let (accounts, scope) = selected();
    let drops = Arc::new(AtomicUsize::new(0));
    install(&accounts, scope, Arc::clone(&drops));
    let work = accounts
        .begin(scope, Request::ReadStatus, Instant::now())
        .unwrap();
    accounts.close();
    assert!(!accounts.closed_and_drained());
    drop(work);
    assert!(accounts.closed_and_drained());
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::StorageUnconfirmed
    );
    drop(accounts);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn profile_removal_is_sticky_and_an_enabled_history_never_uses_map_emptiness() {
    let (accounts, scope) = selected();
    accounts.profiles_changed(&[], None);
    accounts.profiles_changed(&[scope.profile], Some(scope.profile));
    assert_eq!(accounts.current_scope(), Err(Error::Stale));
    let unknown = TrialAccounts::new();
    unknown.inner.state().closed_gate_without_history = false;
    assert_eq!(
        unknown.retire_profile(profile()),
        Retirement::StorageUnconfirmed
    );
}
#[test]
fn actual_owner_profile_mismatch_is_refused_before_resource_installation() {
    let (_, scope) = selected();
    let (_, foreign) = selected();
    let drops = Arc::new(AtomicUsize::new(0));
    let result = Resource::new(
        scope,
        owner(foreign),
        TestPort {
            drops: Arc::clone(&drops),
            run: |_: &account::Control| Ok(Reply::Status(AuthStatus::SignedOut)),
        },
    );
    assert!(matches!(result, Err(Error::Stale)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn in_flight_preparation_cannot_revive_after_a_b_a_then_new_epoch_can_prepare() {
    let (accounts, scope) = selected();
    let preparation = accounts.prepare(scope).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let returned_drops = Arc::clone(&drops);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        preparation.run(&mut TestFactory(|control: &PreparationControl| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(control.cancelled(Instant::now()));
            Ok(resource(control.scope(), Arc::clone(&returned_drops)))
        }))
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let b = profile();
    accounts.profiles_changed(&[scope.profile, b], Some(b));
    accounts.profiles_changed(&[scope.profile, b], Some(scope.profile));
    let new_scope = accounts.current_scope().unwrap();
    assert_ne!(new_scope, scope);
    assert!(matches!(accounts.prepare(new_scope), Err(Error::Busy)));
    release_tx.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Err(Error::Stale));
    assert!(matches!(
        accounts.begin(new_scope, Request::ReadStatus, Instant::now()),
        Err(Error::Stale)
    ));
    install(&accounts, new_scope, Arc::clone(&drops));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let state = accounts.inner.state();
    let owner = &state.entries[&scope.profile]
        .resource
        .as_ref()
        .unwrap()
        .owner;
    assert_eq!(owner.view().status, None);
    assert!(!owner.view().closed);
}

#[test]
fn preparation_deadline_begins_at_reservation_and_expiry_prevents_factory_entry() {
    let (accounts, scope) = selected();
    let start = Instant::now();
    let preparation = accounts.prepare_at(scope, start).unwrap();
    assert_eq!(preparation.control.deadline(), start + PREPARATION_LIMIT);
    let calls = AtomicUsize::new(0);
    let result = preparation.run_at(
        &mut TestFactory(|_: &PreparationControl| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Error::Preparation)
        }),
        || start + PREPARATION_LIMIT,
    );
    assert_eq!(result, Err(Error::Expired));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    accounts.close();
    assert!(accounts.closed_and_drained());
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::StorageUnconfirmed
    );
}
#[test]
fn expired_blocking_preparation_is_retained_until_return_then_cannot_install() {
    use std::cell::Cell;
    let (accounts, scope) = selected();
    let start = Instant::now();
    let preparation = accounts.prepare_at(scope, start).unwrap();
    let returned = Cell::new(false);
    let drops = Arc::new(AtomicUsize::new(0));
    let result = preparation.run_at(
        &mut TestFactory(|control: &PreparationControl| {
            assert!(!control.cancelled(start));
            assert!(control.cancelled(start + PREPARATION_LIMIT));
            // An ignored cancellation/deadline does not discharge the record.
            assert!(accounts.inner.state().entries[&scope.profile]
                .preparation
                .is_some());
            returned.set(true);
            Ok(resource(control.scope(), Arc::clone(&drops)))
        }),
        || {
            if returned.get() {
                start + PREPARATION_LIMIT
            } else {
                start
            }
        },
    );
    assert_eq!(result, Err(Error::Expired));
    assert!(
        accounts.inner.state().entries[&scope.profile]
            .resource
            .as_ref()
            .unwrap()
            .owner
            .view()
            .closed
    );
    assert_eq!(
        accounts.retire_profile(scope.profile),
        Retirement::StorageUnconfirmed
    );
    accounts.close();
    assert!(accounts.closed_and_drained());
    drop(accounts);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn preparation_uses_the_same_global_admission_as_actual_account_work() {
    let (accounts, scope) = selected();
    let foreign = owner(scope);
    let preparation = accounts.prepare(scope).unwrap();
    assert!(matches!(
        foreign.begin(
            foreign.scope().unwrap(),
            Request::ReadStatus,
            Instant::now()
        ),
        Err(account::Error::Busy)
    ));
    let other_registry = TrialAccounts::new();
    other_registry.profiles_changed(&[scope.profile], Some(scope.profile));
    assert!(matches!(
        other_registry.prepare(other_registry.current_scope().unwrap()),
        Err(Error::Busy)
    ));
    drop(preparation);
    let work = foreign
        .begin(
            foreign.scope().unwrap(),
            Request::ReadStatus,
            Instant::now(),
        )
        .unwrap();
    assert!(matches!(accounts.prepare(scope), Err(Error::Busy)));
    drop(work);
    drop(accounts.prepare(scope).unwrap());
}
#[test]
fn preparation_unwind_retains_global_admission_not_only_registry_state() {
    let (accounts, scope) = selected();
    let preparation = accounts.prepare(scope).unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _ = preparation.run(&mut TestFactory(
            |_: &PreparationControl| -> Result<Resource, Error> {
                panic!("synthetic admission unwind")
            },
        ));
    }))
    .is_err());
    drop(accounts);
    let foreign = owner(scope);
    assert!(matches!(
        foreign.begin(
            foreign.scope().unwrap(),
            Request::ReadStatus,
            Instant::now()
        ),
        Err(account::Error::Busy)
    ));
}

#[test]
fn returned_wrapper_does_not_retain_custody_of_a_newer_operation() {
    let (accounts, scope) = selected();
    let weak = Arc::downgrade(&accounts.inner);
    let drops = Arc::new(AtomicUsize::new(0));
    install(&accounts, scope, Arc::clone(&drops));
    let mut older = accounts
        .begin(scope, Request::ReadStatus, Instant::now())
        .unwrap();
    // Execute the same actual AccountOwner/Port path, keeping only this outer
    // wrapper alive so the newer admission deterministically precedes its Drop.
    assert_eq!(older.run_inner(), Ok(Reply::Status(AuthStatus::SignedOut)));
    let newer = accounts
        .begin(scope, Request::ReadStatus, Instant::now())
        .unwrap();
    drop(older);
    accounts.close();
    drop(accounts);
    assert!(weak.upgrade().is_some()); // The newer queued Work still owns it.
    drop(newer); // Never entered; it has no native settlement obligation.
    assert!(weak.upgrade().is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn never_entered_wrapper_does_not_retain_custody_of_a_newer_operation() {
    let (accounts, scope) = selected();
    let weak = Arc::downgrade(&accounts.inner);
    let drops = Arc::new(AtomicUsize::new(0));
    install(&accounts, scope, Arc::clone(&drops));
    let mut older = accounts
        .begin(scope, Request::ReadStatus, Instant::now())
        .unwrap();
    // Interpose at exactly Drop's existing never-entered account::Work release.
    drop(older.work.take());
    let newer = accounts
        .begin(scope, Request::ReadStatus, Instant::now())
        .unwrap();
    drop(older);
    accounts.close();
    drop(accounts);
    drop(newer);
    assert!(weak.upgrade().is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
