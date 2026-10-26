use super::*;
use crate::protocol::Completion;
use std::{cell::Cell, panic::AssertUnwindSafe, time::Duration};

impl<F> Port for F
where
    F: FnMut(Request, &Control) -> Result<Reply, Error>,
{
    fn run(&mut self, request: Request, control: &Control) -> Result<Reply, Error> {
        self(request, control)
    }
}
fn native_owner() -> Owner {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 40);
    Owner {
        profile: u128::from(NEXT.fetch_add(1, Ordering::Relaxed)),
        account_generation: 1,
        profile_generation: 1,
    }
}
fn owner() -> AccountOwner {
    AccountOwner::new(native_owner()).unwrap()
}
fn turn(selected: AuthStatus) -> Request {
    Request::Turn {
        input: TurnInput::new("synthetic user turn").unwrap(),
        selected,
    }
}
fn complete(work: Work, reply: Reply, now: Instant) -> Result<Reply, Error> {
    let mut reply = Some(reply);
    work.run(
        &mut |_: Request, _: &Control| Ok(reply.take().unwrap()),
        || now,
    )
}
fn status(owner: &AccountOwner, value: AuthStatus, now: Instant) -> Scope {
    let work = owner
        .begin(owner.scope().unwrap(), Request::ReadStatus, now)
        .unwrap();
    assert_eq!(
        complete(work, Reply::Status(value), now),
        Ok(Reply::Status(value))
    );
    owner.scope().unwrap()
}
fn outcome() -> Reply {
    Reply::Turn(TurnOutcome {
        completion: Completion::Succeeded,
        text: "synthetic reply".into(),
        api_error_status: None,
    })
}
fn request(op: Operation) -> Request {
    match op {
        Operation::ReadStatus => Request::ReadStatus,
        Operation::StartLogin => Request::Login,
        Operation::Logout => Request::Logout,
        Operation::TextTurn => turn(AuthStatus::ClaudeAccount),
    }
}

#[test]
fn every_operation_serializes_against_every_other_operation() {
    for first in [
        Operation::ReadStatus,
        Operation::StartLogin,
        Operation::Logout,
        Operation::TextTurn,
    ] {
        for second in [
            Operation::ReadStatus,
            Operation::StartLogin,
            Operation::Logout,
            Operation::TextTurn,
        ] {
            let owner = owner();
            let now = Instant::now();
            let scope = status(&owner, AuthStatus::ClaudeAccount, now);
            let work = owner.begin(scope, request(first), now).unwrap();
            assert_eq!(
                owner
                    .begin(owner.scope().unwrap(), request(second), now)
                    .err(),
                Some(Error::Busy)
            );
            assert_eq!(owner.view().operation, Some(first));
            assert!(!owner.drained());
            drop(work); // Port was never entered: this is the only drop-based release.
            assert!(owner.drained());
        }
    }
}
#[test]
fn login_and_logout_success_require_original_status_readback() {
    for (request, reply) in [
        (Request::Login, Reply::LoginFinished),
        (Request::Logout, Reply::LogoutFinished),
    ] {
        let owner = owner();
        let now = Instant::now();
        let old = status(&owner, AuthStatus::ClaudeAccount, now);
        let work = owner.begin(old, request, now).unwrap();
        assert_eq!(owner.view().status, None);
        complete(work, reply, now).unwrap();
        assert_eq!(owner.view().status, None);
        assert_eq!(
            owner
                .begin(owner.scope().unwrap(), turn(AuthStatus::ClaudeAccount), now)
                .err(),
            Some(Error::StatusRequired)
        );
        assert_eq!(
            owner.begin(old, Request::ReadStatus, now).err(),
            Some(Error::Stale)
        );
    }
}
#[test]
fn status_preserves_original_auth_modes_without_entitlement_claims() {
    for auth in [
        AuthStatus::SignedOut,
        AuthStatus::ClaudeAccount,
        AuthStatus::ApiCredential,
        AuthStatus::ExternalProvider,
        AuthStatus::Unknown,
    ] {
        let owner = owner();
        let now = Instant::now();
        let scope = status(&owner, auth, now);
        assert_eq!(owner.view().status, Some(auth));
        let work = owner.begin(scope, turn(auth), now);
        if matches!(auth, AuthStatus::SignedOut | AuthStatus::Unknown) {
            assert_eq!(work.err(), Some(Error::StatusRequired));
        } else {
            complete(work.unwrap(), outcome(), now).unwrap();
        }
    }
}
#[test]
fn requested_subscription_cannot_fall_back_to_api_or_external_auth() {
    for actual in [AuthStatus::ApiCredential, AuthStatus::ExternalProvider] {
        let owner = owner();
        let now = Instant::now();
        let scope = status(&owner, actual, now);
        assert_eq!(
            owner
                .begin(scope, turn(AuthStatus::ClaudeAccount), now)
                .err(),
            Some(Error::StatusRequired)
        );
        assert!(owner.drained());
    }
}
#[test]
fn repeated_explicit_turns_use_the_current_native_admission_revision() {
    let owner = owner();
    let now = Instant::now();
    let scope = status(&owner, AuthStatus::ClaudeAccount, now);
    for _ in 0..3 {
        complete(
            owner
                .begin(scope, turn(AuthStatus::ClaudeAccount), now)
                .unwrap(),
            outcome(),
            now,
        )
        .unwrap();
    }
    assert_eq!(owner.view().status, Some(AuthStatus::ClaudeAccount));
}
#[test]
fn cancellation_is_not_settlement_and_never_retries() {
    let owner = owner();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    let ticket = work.ticket();
    let calls = Cell::new(0);
    let result = work.run(
        &mut |_: Request, control: &Control| {
            calls.set(calls.get() + 1);
            owner.cancel(ticket).unwrap();
            assert!(control.cancelled(now));
            owner.stop_unconfirmed(ticket).unwrap();
            assert_eq!(owner.view().phase, Some(Phase::StopUnconfirmed));
            assert!(!owner.drained());
            assert_eq!(
                owner
                    .begin(owner.scope().unwrap(), Request::Logout, now)
                    .err(),
                Some(Error::Busy)
            );
            Ok(Reply::LoginFinished)
        },
        || now,
    );
    assert_eq!(result, Err(Error::Cancelled));
    assert_eq!(calls.get(), 1);
    assert!(owner.drained());
    assert_eq!(owner.view().status, None);
}
#[test]
fn pre_start_cancel_never_invokes_port_and_releases_only_when_work_drops() {
    let owner = owner();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    owner.cancel(work.ticket()).unwrap();
    assert!(!owner.drained());
    let calls = Cell::new(0);
    let result = work.run(
        &mut |_: Request, _: &Control| {
            calls.set(1);
            Ok(Reply::LoginFinished)
        },
        || now,
    );
    assert_eq!(result, Err(Error::Cancelled));
    assert_eq!(calls.get(), 0);
    assert!(owner.drained());
}
#[test]
fn deadline_starts_at_reservation_before_preparation() {
    let owner = owner();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::ReadStatus, now)
        .unwrap();
    let deadline = now + Duration::from_secs(10);
    owner.expire(deadline);
    assert_eq!(owner.view().phase, Some(Phase::Stopping));
    assert!(!owner.drained());
    let calls = Cell::new(0);
    let result = work.run(
        &mut |_: Request, _: &Control| {
            calls.set(1);
            Ok(Reply::Status(AuthStatus::ClaudeAccount))
        },
        || deadline,
    );
    assert_eq!(result, Err(Error::Expired));
    assert_eq!(calls.get(), 0);
    assert!(owner.drained());
}
#[test]
fn late_status_after_deadline_is_not_connected() {
    let owner = owner();
    let now = Instant::now();
    let clock = Cell::new(now);
    let work = owner
        .begin(owner.scope().unwrap(), Request::ReadStatus, now)
        .unwrap();
    let result = work.run(
        &mut |_: Request, control: &Control| {
            clock.set(control.deadline());
            assert!(control.cancelled(clock.get()));
            assert!(!owner.drained());
            Ok(Reply::Status(AuthStatus::ClaudeAccount))
        },
        || clock.get(),
    );
    assert_eq!(result, Err(Error::Expired));
    assert_eq!(owner.view().status, None);
}
#[test]
fn queued_profile_bounce_cannot_start_old_work() {
    let owner = owner();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    owner.set_foreground(false);
    owner.set_foreground(true);
    let calls = Cell::new(0);
    let result = work.run(
        &mut |_: Request, _: &Control| {
            calls.set(1);
            Ok(Reply::LoginFinished)
        },
        || now,
    );
    assert_eq!(result, Err(Error::Stale));
    assert_eq!(calls.get(), 0);
    assert!(owner.drained());
}
#[test]
fn in_flight_profile_bounce_fences_result_but_keeps_custody_until_return() {
    let owner = owner();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::ReadStatus, now)
        .unwrap();
    let result = work.run(
        &mut |_: Request, control: &Control| {
            owner.set_foreground(false);
            owner.set_foreground(true);
            assert!(control.cancelled(now));
            assert!(!owner.drained());
            assert_eq!(
                owner
                    .begin(owner.scope().unwrap(), Request::Login, now)
                    .err(),
                Some(Error::Busy)
            );
            Ok(Reply::Status(AuthStatus::ClaudeAccount))
        },
        || now,
    );
    assert_eq!(result, Err(Error::Stale));
    assert_eq!(owner.view().status, None);
    assert!(owner.drained());
}
#[test]
fn close_is_sticky_and_repeated_close_does_not_release_running_work() {
    let owner = owner();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    let result = work.run(
        &mut |_: Request, control: &Control| {
            owner.close();
            owner.close();
            owner.set_foreground(true);
            assert!(control.cancelled(now));
            assert!(!owner.drained());
            assert_eq!(owner.scope(), Err(Error::Closed));
            Ok(Reply::LoginFinished)
        },
        || now,
    );
    assert_eq!(result, Err(Error::Closed));
    assert!(owner.drained());
    assert!(owner.view().closed);
}
#[test]
fn foreign_scope_and_ticket_do_not_cancel_or_observe_another_operation() {
    let left = owner();
    let right = owner();
    let now = Instant::now();
    assert_eq!(
        left.begin(right.scope().unwrap(), Request::Login, now)
            .err(),
        Some(Error::Stale)
    );
    let a = left
        .begin(left.scope().unwrap(), Request::Login, now)
        .unwrap();
    let b = right
        .begin(right.scope().unwrap(), Request::Login, now)
        .unwrap();
    assert_eq!(left.cancel(b.ticket()), Err(Error::Stale));
    assert_eq!(right.stop_unconfirmed(a.ticket()), Err(Error::Stale));
    assert_eq!(left.view().phase, Some(Phase::Preparing));
}
#[test]
fn old_completion_ticket_does_not_cancel_a_new_operation() {
    let owner = owner();
    let now = Instant::now();
    let a = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    let old = a.ticket();
    complete(a, Reply::LoginFinished, now).unwrap();
    let _b = owner
        .begin(owner.scope().unwrap(), Request::ReadStatus, now)
        .unwrap();
    assert_eq!(owner.cancel(old), Err(Error::Stale));
    assert_eq!(owner.view().phase, Some(Phase::Preparing));
}
#[test]
fn original_login_controller_and_all_operations_share_the_same_slot() {
    let identity = native_owner();
    let owner = AccountOwner::new(identity).unwrap();
    let now = Instant::now();
    let mut legacy = auth::Controller::new(identity).unwrap();
    let (attempt, input, _) = legacy.reserve(identity, now).unwrap();
    assert_eq!(
        owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .err(),
        Some(Error::Busy)
    );
    legacy.abandon_uncreated(attempt).unwrap();
    drop(input);
    let work = owner
        .begin(owner.scope().unwrap(), Request::ReadStatus, now)
        .unwrap();
    assert_eq!(
        legacy.reserve(identity, now).err(),
        Some(auth::AuthError::Busy)
    );
    drop(work);
    assert!(legacy.reserve(identity, now).is_ok());
}
#[test]
fn original_login_retirement_invalidates_old_status_even_after_lease_is_released() {
    let identity = native_owner();
    let owner = AccountOwner::new(identity).unwrap();
    let now = Instant::now();
    let old = status(&owner, AuthStatus::ClaudeAccount, now);
    let mut legacy = auth::Controller::new(identity).unwrap();
    let (attempt, input, _) = legacy.reserve(identity, now).unwrap();
    assert_eq!(owner.view().status, None);
    legacy.abandon_uncreated(attempt).unwrap();
    drop(input);
    assert_eq!(
        owner.begin(old, turn(AuthStatus::ClaudeAccount), now).err(),
        Some(Error::StatusRequired)
    );
    assert_eq!(owner.view().status, None);
}
#[test]
fn separate_native_owner_cannot_reuse_pre_mutation_account_status() {
    let identity = native_owner();
    let left = AccountOwner::new(identity).unwrap();
    let right = AccountOwner::new(identity).unwrap();
    let now = Instant::now();
    let old = status(&left, AuthStatus::ClaudeAccount, now);
    status(&right, AuthStatus::ApiCredential, now);
    assert_eq!(left.view().status, None);
    assert_eq!(
        left.begin(old, turn(AuthStatus::ClaudeAccount), now).err(),
        Some(Error::StatusRequired)
    );
    assert!(right
        .begin(right.scope().unwrap(), turn(AuthStatus::ApiCredential), now)
        .is_ok());
}
#[test]
fn dropping_native_owner_cancels_but_worker_keeps_the_profile_slot() {
    let identity = native_owner();
    let mut owner = Some(AccountOwner::new(identity).unwrap());
    let now = Instant::now();
    let work = owner
        .as_ref()
        .unwrap()
        .begin(
            owner.as_ref().unwrap().scope().unwrap(),
            Request::Login,
            now,
        )
        .unwrap();
    let replacement = AccountOwner::new(identity).unwrap();
    let result = work.run(
        &mut |_: Request, control: &Control| {
            drop(owner.take());
            assert!(control.cancelled(now));
            assert_eq!(
                replacement
                    .begin(replacement.scope().unwrap(), Request::Logout, now)
                    .err(),
                Some(Error::Busy)
            );
            Ok(Reply::LoginFinished)
        },
        || now,
    );
    assert_eq!(result, Err(Error::Closed));
    assert!(replacement
        .begin(replacement.scope().unwrap(), Request::ReadStatus, now)
        .is_ok());
}
#[test]
fn panic_is_not_settlement_and_cannot_release_admission_by_dropping_views() {
    let identity = native_owner();
    let owner = AccountOwner::new(identity).unwrap();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        work.run(
            &mut |_: Request, _: &Control| -> Result<Reply, Error> {
                panic!("synthetic worker panic")
            },
            || now,
        )
    }));
    assert!(result.is_err());
    assert!(!owner.drained());
    assert_eq!(owner.view().phase, Some(Phase::StopUnconfirmed));
    assert!(owner.view().closed);
    drop(owner);
    let replacement = AccountOwner::new(identity).unwrap();
    assert_eq!(
        replacement
            .begin(replacement.scope().unwrap(), Request::Logout, now)
            .err(),
        Some(Error::Busy)
    );
}
#[test]
fn wrong_operation_reply_is_rejected_without_status_or_retry() {
    let owner = owner();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    assert_eq!(
        complete(work, Reply::Status(AuthStatus::ClaudeAccount), now),
        Err(Error::Protocol)
    );
    assert_eq!(owner.view().status, None);
    assert!(owner.drained());
}
#[test]
fn settled_process_failure_is_not_a_successful_logout() {
    let owner = owner();
    let now = Instant::now();
    let scope = status(&owner, AuthStatus::ClaudeAccount, now);
    let work = owner.begin(scope, Request::Logout, now).unwrap();
    let calls = Cell::new(0);
    let result = work.run(
        &mut |_: Request, _: &Control| {
            calls.set(calls.get() + 1);
            Err(Error::Process)
        },
        || now,
    );
    assert_eq!(result, Err(Error::Process));
    assert_eq!(calls.get(), 1);
    assert_eq!(owner.view().status, None);
    assert!(owner.drained());
}
#[test]
fn request_text_stays_on_bounded_transient_input_not_account_projection() {
    let owner = owner();
    let now = Instant::now();
    let scope = status(&owner, AuthStatus::ClaudeAccount, now);
    let work = owner
        .begin(scope, turn(AuthStatus::ClaudeAccount), now)
        .unwrap();
    let result = work
        .run(
            &mut |request: Request, _: &Control| {
                let Request::Turn { input, .. } = request else {
                    panic!("unexpected operation")
                };
                assert_eq!(input.as_bytes(), b"synthetic user turn");
                assert!(!format!("{:?}", owner.view()).contains("synthetic"));
                Ok(outcome())
            },
            || now,
        )
        .unwrap();
    assert!(!format!("{result:?}").contains("synthetic reply"));
}

#[test]
fn concurrent_native_owners_admit_exactly_one_physical_operation() {
    let identity = native_owner();
    let left = AccountOwner::new(identity).unwrap();
    let right = AccountOwner::new(identity).unwrap();
    let enter = std::sync::Barrier::new(3);
    let release = std::sync::Barrier::new(3);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|threads| {
        for owner in [&left, &right] {
            let tx = tx.clone();
            let enter = &enter;
            let release = &release;
            threads.spawn(move || {
                enter.wait();
                let result =
                    owner.begin(owner.scope().unwrap(), Request::ReadStatus, Instant::now());
                tx.send(result.as_ref().map(|_| ()).map_err(|e| *e))
                    .unwrap();
                release.wait(); // Hold any admitted Work until both attempts finish.
                drop(result);
            });
        }
        enter.wait();
        let results = [rx.recv().unwrap(), rx.recv().unwrap()];
        release.wait();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results.iter().filter(|r| **r == Err(Error::Busy)).count(),
            1
        );
    });
    assert!(left.drained() && right.drained());
}
#[test]
fn background_worker_remains_owned_after_foreground_shutdown_until_actual_return() {
    let identity = native_owner();
    let owner = AccountOwner::new(identity).unwrap();
    let replacement = AccountOwner::new(identity).unwrap();
    let now = Instant::now();
    let work = owner
        .begin(owner.scope().unwrap(), Request::Login, now)
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::scope(|threads| {
        let worker = threads.spawn(move || {
            work.run(
                &mut |_: Request, control: &Control| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    assert!(control.cancelled(now));
                    Ok(Reply::LoginFinished)
                },
                || now,
            )
        });
        entered_rx.recv().unwrap();
        owner.close();
        assert!(!owner.drained());
        assert_eq!(
            replacement
                .begin(replacement.scope().unwrap(), Request::ReadStatus, now)
                .err(),
            Some(Error::Busy)
        );
        release_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), Err(Error::Closed));
    });
    assert!(owner.drained());
    assert!(replacement
        .begin(replacement.scope().unwrap(), Request::ReadStatus, now)
        .is_ok());
}
#[test]
fn operation_and_account_revision_exhaustion_never_wrap_into_old_authority() {
    let owner = owner();
    let now = Instant::now();
    let scope = owner.scope().unwrap();
    owner.inner.state().next = u64::MAX;
    assert_eq!(
        owner.begin(scope, Request::Login, now).err(),
        Some(Error::Exhausted)
    );
    assert!(owner.drained());
    {
        let mut state = owner.inner.state();
        state.next = 0;
        state.scope.account = u64::MAX;
    }
    assert_eq!(
        owner
            .begin(owner.scope().unwrap(), Request::Login, now)
            .err(),
        Some(Error::Exhausted)
    );
    assert!(owner.drained());
    owner.inner.state().scope.epoch = u64::MAX;
    owner.set_foreground(false);
    owner.set_foreground(true);
    assert_eq!(owner.scope(), Err(Error::Closed));
}

#[test]
fn contradictory_turn_reply_invalidates_cached_auth_before_any_next_turn() {
    let owner = owner();
    let now = Instant::now();
    let scope = status(&owner, AuthStatus::ClaudeAccount, now);
    let work = owner
        .begin(scope, turn(AuthStatus::ClaudeAccount), now)
        .unwrap();
    assert_eq!(
        complete(work, Reply::Status(AuthStatus::ApiCredential), now),
        Err(Error::Protocol)
    );
    assert_eq!(owner.view().status, None);
    assert_eq!(
        owner
            .begin(scope, turn(AuthStatus::ClaudeAccount), now)
            .err(),
        Some(Error::StatusRequired)
    );
}
#[test]
fn settled_turn_failure_does_not_reuse_an_unverified_cached_auth_mode() {
    let owner = owner();
    let now = Instant::now();
    let scope = status(&owner, AuthStatus::ClaudeAccount, now);
    let work = owner
        .begin(scope, turn(AuthStatus::ClaudeAccount), now)
        .unwrap();
    assert_eq!(
        work.run(&mut |_: Request, _: &Control| Err(Error::Process), || now),
        Err(Error::Process)
    );
    assert_eq!(owner.view().status, None);
    assert_eq!(
        owner
            .begin(scope, turn(AuthStatus::ClaudeAccount), now)
            .err(),
        Some(Error::StatusRequired)
    );
}

#[test]
fn provider_auth_rejection_or_interrupted_terminal_requires_status_readback() {
    for (completion, api_error_status) in [
        (Completion::ProviderError, Some(401)),
        (Completion::ProviderError, Some(403)),
        (Completion::ProviderError, Some(500)),
        (Completion::Interrupted, None),
    ] {
        let owner = owner();
        let now = Instant::now();
        let scope = status(&owner, AuthStatus::ClaudeAccount, now);
        let work = owner
            .begin(scope, turn(AuthStatus::ClaudeAccount), now)
            .unwrap();
        let result = complete(
            work,
            Reply::Turn(TurnOutcome {
                completion,
                text: String::new(),
                api_error_status,
            }),
            now,
        )
        .unwrap();
        assert!(matches!(result, Reply::Turn(outcome) if outcome.completion == completion));
        assert_eq!(owner.view().status, None);
        assert_eq!(
            owner
                .begin(scope, turn(AuthStatus::ClaudeAccount), now)
                .err(),
            Some(Error::StatusRequired)
        );
    }
}
#[test]
fn post_port_cancel_and_expiry_invalidate_cached_turn_auth() {
    for expire in [false, true] {
        let owner = owner();
        let now = Instant::now();
        let clock = Cell::new(now);
        let scope = status(&owner, AuthStatus::ClaudeAccount, now);
        let work = owner
            .begin(scope, turn(AuthStatus::ClaudeAccount), now)
            .unwrap();
        let ticket = work.ticket();
        let result = work.run(
            &mut |_: Request, control: &Control| {
                if expire {
                    clock.set(control.deadline());
                } else {
                    owner.cancel(ticket).unwrap();
                }
                Ok(outcome())
            },
            || clock.get(),
        );
        assert_eq!(
            result,
            Err(if expire {
                Error::Expired
            } else {
                Error::Cancelled
            })
        );
        assert_eq!(owner.view().status, None);
        assert_eq!(
            owner
                .begin(scope, turn(AuthStatus::ClaudeAccount), clock.get())
                .err(),
            Some(Error::StatusRequired)
        );
    }
}

#[test]
fn native_control_binds_the_complete_owner_tuple_without_changing_deadline() {
    for (account_generation, profile_generation) in [(1, 2), (13, 17)] {
        let expected = Owner {
            account_generation,
            profile_generation,
            ..native_owner()
        };
        let owner = AccountOwner::new(expected).unwrap();
        let now = Instant::now();
        let work = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        let expected_deadline = now + CommandPlan::for_operation(Operation::ReadStatus).deadline();
        let result = work.run(
            &mut |_: Request, control: &Control| {
                assert_eq!(control.owner(), expected);
                assert_eq!(control.deadline(), expected_deadline);
                assert!(!control.cancelled(now));
                Ok(Reply::Status(AuthStatus::SignedOut))
            },
            || now,
        );
        assert_eq!(result, Ok(Reply::Status(AuthStatus::SignedOut)));
    }
}
