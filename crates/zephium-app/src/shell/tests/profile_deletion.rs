use super::*;

fn insert_inactive_profile_tab(
    shell: &mut Shell,
    profile: ProfileId,
    seed: u128,
) -> (SpaceId, ItemId) {
    let space = shell
        .spaces
        .iter()
        .find(|space| space.profile == profile)
        .expect("inactive profile owns its test space")
        .id;
    let item = ItemId::from(seed);
    assert!(shell.items.insert_tab(
        item,
        Placement::Space {
            space,
            section: SpaceSection::Today,
        },
    ));
    (space, item)
}

#[test]
fn profile_deletion_completes_once_only_after_native_and_store_phases() {
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _screen, operations) =
        setup_with_operation_log_and_failure(store.clone(), Box::new(|_| {}));
    shell.handle(Command::Bootstrap);
    store.events.lock().unwrap().clear();
    let profile = add_inactive_named_profile(&mut shell, 20_000);
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);

    shell.handle(delete_operation("delete-1", profile));

    let completions = operations.lock().unwrap().clone();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].operation_id, "delete-1");
    assert_eq!(completions[0].outcome, OperationOutcome::Applied);
    assert_eq!(
        completions[0].reason,
        OperationReason::ProfileDeletionCompleted
    );
    assert!(shell.profiles.get(profile).is_none());
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    let authorized = store.authorized_sessions.lock().unwrap();
    assert_eq!(authorized.len(), 1);
    assert!(authorized[0]
        .1
        .profiles
        .iter()
        .all(|candidate| candidate.id != profile));
    assert_eq!(
        session::canonicalize(authorized[0].1.clone()),
        authorized[0].1
    );
    assert_eq!(
        store.events.lock().unwrap().as_slice(),
        ["authorize-delete", "finalize-delete"]
    );
    assert_eq!(engine.erasure_requests(), vec![profile]);
}

#[test]
fn profile_with_a_native_view_obligation_is_rejected_before_authorization() {
    let store = Arc::new(FakeStore::default());
    let (mut shell, _engine, _screen, operations) =
        setup_with_operation_log_and_failure(store.clone(), Box::new(|_| {}));
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 20_050);
    let (_space, item) = insert_inactive_profile_tab(&mut shell, profile, 20_052);
    let effects = shell
        .items
        .navigate(item, "https://view-obligation.example/");
    assert!(matches!(effects.as_slice(), [Effect::CreateView { id, .. }] if *id == item));
    assert!(shell.items.tab(item).is_some_and(TabState::has_view));

    shell.handle(delete_operation("delete-with-live-view", profile));

    assert!(!shell.profile_deletion.states.contains_key(&profile));
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    let completions = operations.lock().unwrap();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].outcome, OperationOutcome::Rejected);
    assert_eq!(
        completions[0].reason,
        OperationReason::ProfileDeletionPolicyRejected
    );
}

#[test]
fn impossible_store_authorization_outcomes_are_terminal_invariants() {
    for (offset, outcome) in [
        ProfileDeletionAuthorizeOutcome::NotRegistered,
        ProfileDeletionAuthorizeOutcome::SessionConflict,
        ProfileDeletionAuthorizeOutcome::InvalidSession,
    ]
    .into_iter()
    .enumerate()
    {
        let store = Arc::new(FakeStore::default());
        store.authorize_outcomes.lock().unwrap().push_back(outcome);
        let failures = Arc::new(Mutex::new(Vec::new()));
        let (mut shell, engine, _screen, operations) =
            setup_with_operation_log_and_failure(store.clone(), {
                let failures = Arc::clone(&failures);
                Box::new(move |failure| failures.lock().unwrap().push(failure))
            });
        shell.handle(Command::Bootstrap);
        let profile = add_inactive_named_profile(&mut shell, 20_300 + offset as u128 * 10);

        shell.handle(delete_operation("delete-store-conflict", profile));

        assert_eq!(
            failures.lock().unwrap().as_slice(),
            &[ShellTerminalFailure::ProfileDeletionInvariant],
            "{outcome:?}"
        );
        assert!(shell.profiles.get(profile).is_some(), "{outcome:?}");
        assert!(
            store.pending_deletions.lock().unwrap().is_empty(),
            "{outcome:?}"
        );
        assert!(!engine
            .calls()
            .iter()
            .any(|call| call == &format!("erase-profile {profile}")));
        assert!(operations.lock().unwrap().is_empty(), "{outcome:?}");
    }
}

#[test]
fn untracked_profile_deletion_command_is_inert() {
    let store = Arc::new(FakeStore::default());
    let (mut shell, engine, _screen) = setup_with(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 20_500);

    shell.handle(Command::DeleteProfile(profile));

    assert!(shell.profiles.get(profile).is_some());
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert!(!engine
        .calls()
        .iter()
        .any(|call| call == &format!("erase-profile {profile}")));
}

#[test]
fn profile_deletion_retry_emits_no_intermediate_or_duplicate_completion() {
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _screen, operations) = setup_with_operation_log(store);
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 21_000);
    engine.push_erasure_outcomes([
        ProfileDataErasureOutcome::TimedOut,
        ProfileDataErasureOutcome::Verified,
    ]);

    shell.handle(delete_operation("delete-retry", profile));
    assert!(operations.lock().unwrap().is_empty());
    assert!(shell.profiles.get(profile).is_none());
    let generation = shell
        .profile_deletion
        .states
        .get(&profile)
        .unwrap()
        .retry_generation;
    shell.handle(Command::ProfileDeletionRetry {
        profile,
        generation,
    });

    let completions = operations.lock().unwrap().clone();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].operation_id, "delete-retry");
    assert_eq!(completions[0].outcome, OperationOutcome::Applied);
}

#[test]
fn duplicate_profile_deletion_is_rejected_without_stealing_original_id() {
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _screen, operations) = setup_with_operation_log(store);
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 22_000);
    engine
        .hold_erasures
        .store(true, std::sync::atomic::Ordering::Release);

    shell.handle(delete_operation("delete-original", profile));
    shell.handle(delete_operation("delete-duplicate", profile));
    {
        let completions = operations.lock().unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].operation_id, "delete-duplicate");
        assert_eq!(completions[0].outcome, OperationOutcome::Rejected);
        assert_eq!(
            completions[0].reason,
            OperationReason::ProfileDeletionInProgress
        );
    }

    engine.complete_held_erasure(ProfileDataErasureOutcome::Verified);
    shell.handle(Command::ProfileDeletionReady(profile));
    let completions = operations.lock().unwrap().clone();
    assert_eq!(completions.len(), 2);
    assert_eq!(
        completions
            .iter()
            .filter(|completion| completion.operation_id == "delete-original")
            .count(),
        1
    );
}

#[test]
fn uncertain_profile_deletion_rpcs_reconcile_from_the_journal() {
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::OutcomeUnknown);
    store
        .authorize_unknown_commits
        .store(true, std::sync::atomic::Ordering::Release);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::OutcomeUnknown);
    store
        .finalize_unknown_completes
        .store(true, std::sync::atomic::Ordering::Release);
    let (mut shell, engine, _screen, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 23_000);
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);

    shell.handle(delete_operation("delete-uncertain", profile));

    let completions = operations.lock().unwrap().clone();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].operation_id, "delete-uncertain");
    assert_eq!(completions[0].outcome, OperationOutcome::Applied);
    assert!(store.pending_deletions.lock().unwrap().is_empty());
}

#[test]
fn uncertain_profile_deletion_reauthorization_rebuilds_the_survivor_snapshot() {
    let store = Arc::new(FakeStore::default());
    store.authorize_outcomes.lock().unwrap().extend([
        ProfileDeletionAuthorizeOutcome::OutcomeUnknown,
        ProfileDeletionAuthorizeOutcome::Authorized,
    ]);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, screen, operations) =
        setup_with_operation_log_and_failure(store.clone(), Box::new(|_| {}));
    shell.handle(Command::Bootstrap);
    let survivor = active_id(&screen);
    let profile = add_inactive_named_profile(&mut shell, 23_500);
    // Fail the ordered journal read after the first durability-ambiguous
    // authorization. This leaves a real actor-order window in which newer
    // survivor mutations can be accepted before a safe retry.
    store
        .pending_load_failures
        .store(1, std::sync::atomic::Ordering::Release);

    shell.handle(delete_operation("delete-rebuild", profile));
    assert!(operations.lock().unwrap().is_empty());
    navigate_and_commit(&mut shell, survivor, "new-survivor.example");
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    let generation = shell
        .profile_deletion
        .states
        .get(&profile)
        .unwrap()
        .retry_generation;
    shell.handle(Command::ProfileDeletionRetry {
        profile,
        generation,
    });
    // A stale timer/callback cannot complete the already-removed state a
    // second time.
    shell.handle(Command::ProfileDeletionRetry {
        profile,
        generation,
    });
    shell.handle(Command::ProfileDeletionReady(profile));

    let authorized = store.authorized_sessions.lock().unwrap();
    assert_eq!(authorized.len(), 2);
    let retried = &authorized[1].1;
    assert!(retried
        .profiles
        .iter()
        .all(|candidate| candidate.id != profile));
    assert!(retried.items.iter().any(|item| {
        item.id == survivor
            && matches!(
                &item.kind,
                PersistedKind::Tab { url, .. } if url == "https://new-survivor.example/"
            )
    }));
    let completions = operations.lock().unwrap();
    assert_eq!(
        completions
            .iter()
            .filter(|completion| completion.operation_id == "delete-rebuild")
            .count(),
        1
    );
    assert_eq!(completions[0].outcome, OperationOutcome::Applied);
}

#[test]
fn delayed_authorization_proof_reschedules_newer_survivor_durability() {
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::OutcomeUnknown);
    store
        .authorize_unknown_commits
        .store(true, std::sync::atomic::Ordering::Release);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, screen, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let survivor = active_id(&screen);
    let profile = add_inactive_named_profile(&mut shell, 23_750);
    store
        .pending_load_failures
        .store(1, std::sync::atomic::Ordering::Release);

    shell.handle(delete_operation("delete-post-barrier", profile));
    navigate_and_commit(&mut shell, survivor, "post-barrier.example");
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    let generation = shell
        .profile_deletion
        .states
        .get(&profile)
        .unwrap()
        .retry_generation;
    shell.handle(Command::ProfileDeletionRetry {
        profile,
        generation,
    });

    let persisted = store.saved.lock().unwrap().clone().unwrap();
    assert!(persisted
        .profiles
        .iter()
        .all(|candidate| candidate.id != profile));
    assert!(persisted.items.iter().any(|item| {
        item.id == survivor
            && matches!(
                &item.kind,
                PersistedKind::Tab { url, .. } if url == "https://post-barrier.example/"
            )
    }));
    let completions = operations.lock().unwrap();
    assert_eq!(
        completions
            .iter()
            .filter(|completion| completion.operation_id == "delete-post-barrier")
            .count(),
        1
    );
    assert_eq!(completions[0].outcome, OperationOutcome::Applied);
}

#[test]
fn restart_resumes_journaled_native_erasure_before_creating_views() {
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    let (mut first, first_engine, screen, first_operations) =
        setup_with_operation_log(store.clone());
    first.handle(Command::Bootstrap);
    let active = active_id(&screen);
    navigate_and_commit(&mut first, active, "survivor.example");
    let profile = add_inactive_named_profile(&mut first, 24_000);
    first_engine.push_erasure_outcomes([ProfileDataErasureOutcome::Failed]);
    first.handle(delete_operation("delete-before-crash", profile));
    assert!(first_operations.lock().unwrap().is_empty());
    assert_eq!(store.pending_deletions.lock().unwrap().len(), 1);
    drop(first);

    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut restarted, engine, _screen) = setup_with(store.clone());
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    restarted.handle(Command::Bootstrap);

    let calls = engine.calls();
    let erase = calls
        .iter()
        .position(|call| call == &format!("erase-profile {profile}"))
        .unwrap();
    let first_create = calls
        .iter()
        .position(|call| call.starts_with("create "))
        .unwrap();
    assert!(erase < first_create);
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    assert!(restarted.profile_deletion.states.is_empty());
}

#[test]
fn restart_with_native_proof_skips_engine_and_finishes_local_purge() {
    let store = Arc::new(FakeStore::default());
    let default_profile = ProfileId::from(25_000);
    let default_space = SpaceId::from(25_001);
    *store.saved.lock().unwrap() = Some(SessionState {
        profiles: vec![PersistedProfile {
            id: default_profile,
            name: "Personal".into(),
            kind: ProfileKind::Default,
        }],
        spaces: vec![PersistedSpace {
            id: default_space,
            profile: default_profile,
            name: "Space".into(),
            look: Default::default(),
        }],
        active_space: Some(default_space),
        ..SessionState::default()
    });
    let removed = ProfileId::from(25_002);
    store
        .pending_deletions
        .lock()
        .unwrap()
        .push(PendingProfileDeletion {
            profile: removed,
            native_erasure_verified: true,
        });
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _screen) = setup_with(store.clone());

    shell.handle(Command::Bootstrap);

    assert!(!engine
        .calls()
        .iter()
        .any(|call| call == &format!("erase-profile {removed}")));
    assert!(store.pending_deletions.lock().unwrap().is_empty());
}

#[test]
fn recovered_finalize_not_authorized_is_a_terminal_invariant() {
    let store = Arc::new(FakeStore::default());
    let default_profile = ProfileId::from(25_300);
    let default_space = SpaceId::from(25_301);
    *store.saved.lock().unwrap() = Some(SessionState {
        profiles: vec![PersistedProfile {
            id: default_profile,
            name: "Personal".into(),
            kind: ProfileKind::Default,
        }],
        spaces: vec![PersistedSpace {
            id: default_space,
            profile: default_profile,
            name: "Space".into(),
            look: Default::default(),
        }],
        active_space: Some(default_space),
        ..SessionState::default()
    });
    let removed = ProfileId::from(25_302);
    store
        .pending_deletions
        .lock()
        .unwrap()
        .push(PendingProfileDeletion {
            profile: removed,
            native_erasure_verified: true,
        });
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::NotAuthorized);
    let failures = Arc::new(Mutex::new(Vec::new()));
    let (mut shell, engine, _screen, _operations) =
        setup_with_operation_log_and_failure(store.clone(), {
            let failures = Arc::clone(&failures);
            Box::new(move |failure| failures.lock().unwrap().push(failure))
        });

    shell.handle(Command::Bootstrap);

    assert_eq!(
        failures.lock().unwrap().as_slice(),
        &[ShellTerminalFailure::ProfileDeletionInvariant]
    );
    assert!(matches!(
        shell.profile_deletion.states[&removed].phase,
        ProfileDeletionPhase::FailedClosed
    ));
    assert!(store
        .pending_deletions
        .lock()
        .unwrap()
        .iter()
        .any(|deletion| deletion.profile == removed));
    assert!(!engine
        .calls()
        .iter()
        .any(|call| call.starts_with("create ")));
}

#[test]
fn retryable_store_authorization_refusals_retain_quarantine() {
    for (offset, first) in [
        ProfileDeletionAuthorizeOutcome::NotAdmitted,
        ProfileDeletionAuthorizeOutcome::Failed,
    ]
    .into_iter()
    .enumerate()
    {
        let store = Arc::new(FakeStore::default());
        store
            .authorize_outcomes
            .lock()
            .unwrap()
            .extend([first, ProfileDeletionAuthorizeOutcome::Authorized]);
        store
            .finalize_outcomes
            .lock()
            .unwrap()
            .push_back(ProfileDeletionFinalizeOutcome::Completed);
        let (mut shell, engine, _screen, operations) =
            setup_with_operation_log_and_failure(store.clone(), Box::new(|_| {}));
        shell.handle(Command::Bootstrap);
        let profile = add_inactive_named_profile(&mut shell, 25_400 + offset as u128 * 10);
        engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);

        shell.handle(delete_operation("retry-store-authorization", profile));

        assert!(shell.profile_deletion_quarantines(profile));
        assert!(shell.profiles.get(profile).is_some());
        assert!(store.pending_deletions.lock().unwrap().is_empty());
        assert!(operations.lock().unwrap().is_empty());
        let generation = shell.profile_deletion.states[&profile].retry_generation;

        shell.handle(Command::ProfileDeletionRetry {
            profile,
            generation,
        });

        assert!(!shell.profile_deletion.states.contains_key(&profile));
        assert!(shell.profiles.get(profile).is_none());
        assert!(store.pending_deletions.lock().unwrap().is_empty());
        assert_eq!(operations.lock().unwrap().len(), 1);
    }
}

#[test]
fn profile_deletion_policy_rejects_default_active_private_and_last_profile() {
    let store = Arc::new(FakeStore::default());
    let (mut shell, _engine, _screen, operations) = setup_with_operation_log(store);
    shell.handle(Command::Bootstrap);
    let default = shell.windows.focused().unwrap().profile;

    shell.handle(delete_operation("delete-default", default));
    let private = ProfileId::from(26_000);
    assert!(shell.profiles.insert(Profile {
        id: private,
        name: "Private".into(),
        kind: ProfileKind::Incognito,
    }));
    shell.handle(delete_operation("delete-private", private));

    let completions = operations.lock().unwrap().clone();
    assert_eq!(completions.len(), 2);
    assert!(completions.iter().all(|completion| {
        completion.outcome == OperationOutcome::Rejected
            && completion.reason == OperationReason::ProfileDeletionPolicyRejected
    }));

    let (mut last_only, _engine, _screen) = setup();
    last_only.bootstrapped = true;
    let only = ProfileId::from(26_100);
    assert!(last_only.profiles.insert(Profile {
        id: only,
        name: "Only".into(),
        kind: ProfileKind::Named,
    }));
    assert!(last_only
        .filtered_session_for_profile_deletion(only)
        .is_none());
}

#[cfg(feature = "work-runtime")]
#[test]
fn legacy_provider_erasure_failure_keeps_deletion_journal_until_retry_succeeds() {
    use crate::work_models::{
        replace_legacy_cleanup_for_test, LegacyProviderCleanup, MODEL_TEST_SERIAL,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    let _guard = MODEL_TEST_SERIAL.blocking_lock();
    struct Account {
        profile: ProfileId,
        erased: AtomicBool,
    }
    impl LegacyProviderCleanup for Account {
        fn erase_profile(&self, profile: ProfileId, _: std::time::Instant) -> bool {
            profile != self.profile || self.erased.load(Ordering::SeqCst)
        }
    }
    struct Restore(Option<Arc<dyn LegacyProviderCleanup>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            replace_legacy_cleanup_for_test(self.0.take());
        }
    }
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _screen, operations) =
        setup_with_operation_log_and_failure(store.clone(), Box::new(|_| {}));
    shell.handle(Command::Bootstrap);
    store.events.lock().unwrap().clear();
    let profile = add_inactive_named_profile(&mut shell, 79_500);
    let account = Arc::new(Account {
        profile,
        erased: AtomicBool::new(false),
    });
    let _restore = Restore(replace_legacy_cleanup_for_test(Some(account.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    shell.handle(delete_operation("delete-subscription", profile));
    assert!(operations.lock().unwrap().is_empty());
    assert!(!store.pending_deletions.lock().unwrap().is_empty());
    assert!(!store.events.lock().unwrap().contains(&"finalize-delete"));
    account.erased.store(true, Ordering::SeqCst);
    shell.drive_profile_deletion(profile);
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    let completions = operations.lock().unwrap();
    assert_eq!(completions.len(), 1);
    assert_eq!(
        completions[0].reason,
        OperationReason::ProfileDeletionCompleted
    );
}

struct NativeRemovalFixture {
    busy: std::sync::atomic::AtomicBool,
    blocked: std::sync::atomic::AtomicBool,
    store: Arc<FakeStore>,
    prepared: std::sync::atomic::AtomicBool,
    cleaned: std::sync::atomic::AtomicBool,
    recovery: std::sync::atomic::AtomicBool,
    wrong_attempt: std::sync::atomic::AtomicBool,
    interrupted: std::sync::atomic::AtomicBool,
    cleanup_interrupted: std::sync::atomic::AtomicBool,
    cleanup_retry_calls: std::sync::atomic::AtomicUsize,
    authorization_interrupted: std::sync::atomic::AtomicBool,
    authorization_pending: std::sync::atomic::AtomicBool,
    authorization_retry_calls: std::sync::atomic::AtomicUsize,
    records: Mutex<Vec<ProfileId>>,
    completed_records: Mutex<Vec<ProfileId>>,
    finalization_interrupted: std::sync::atomic::AtomicBool,
    finalization_ready: std::sync::atomic::AtomicBool,
    finalization_calls: std::sync::atomic::AtomicUsize,
    finalization_retry_calls: std::sync::atomic::AtomicUsize,
    inventory_available: std::sync::atomic::AtomicBool,
    sealed: std::sync::atomic::AtomicUsize,
    retry_calls: std::sync::atomic::AtomicUsize,
    prepare_calls: std::sync::atomic::AtomicUsize,
    recovery_calls: std::sync::atomic::AtomicUsize,
    attempt: Mutex<Option<crate::profile_removal::RemovalAttempt>>,
}
impl NativeRemovalFixture {
    fn new(store: Arc<FakeStore>) -> Arc<Self> {
        Arc::new(Self {
            busy: std::sync::atomic::AtomicBool::new(false),
            blocked: std::sync::atomic::AtomicBool::new(false),
            store,
            prepared: std::sync::atomic::AtomicBool::new(false),
            cleaned: std::sync::atomic::AtomicBool::new(false),
            recovery: std::sync::atomic::AtomicBool::new(false),
            wrong_attempt: std::sync::atomic::AtomicBool::new(false),
            interrupted: std::sync::atomic::AtomicBool::new(false),
            cleanup_interrupted: std::sync::atomic::AtomicBool::new(false),
            cleanup_retry_calls: std::sync::atomic::AtomicUsize::new(0),
            authorization_interrupted: std::sync::atomic::AtomicBool::new(false),
            authorization_pending: std::sync::atomic::AtomicBool::new(false),
            authorization_retry_calls: std::sync::atomic::AtomicUsize::new(0),
            records: Mutex::new(Vec::new()),
            completed_records: Mutex::new(Vec::new()),
            finalization_interrupted: std::sync::atomic::AtomicBool::new(false),
            finalization_ready: std::sync::atomic::AtomicBool::new(true),
            finalization_calls: std::sync::atomic::AtomicUsize::new(0),
            finalization_retry_calls: std::sync::atomic::AtomicUsize::new(0),
            inventory_available: std::sync::atomic::AtomicBool::new(true),
            sealed: std::sync::atomic::AtomicUsize::new(0),
            retry_calls: std::sync::atomic::AtomicUsize::new(0),
            prepare_calls: std::sync::atomic::AtomicUsize::new(0),
            recovery_calls: std::sync::atomic::AtomicUsize::new(0),
            attempt: Mutex::new(None),
        })
    }
}
struct NativeRemovalPort(Arc<NativeRemovalFixture>);
struct NativePrepared {
    owner: Arc<NativeRemovalFixture>,
    attempt: crate::profile_removal::RemovalAttempt,
}
impl crate::profile_removal::PreparedRemoval for NativePrepared {
    fn acknowledge_finalization(
        &mut self,
        finalized: &crate::profile_removal::FinalizedRemoval,
        _: std::time::Instant,
    ) -> crate::profile_removal::CleanupProgress {
        use crate::profile_removal::{CleanupProgress, RemovalStage};
        use std::sync::atomic::Ordering;
        assert!(finalized.attempt().same_attempt(&self.attempt));
        assert!(!self
            .owner
            .store
            .pending_deletions
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.profile == self.attempt.profile()));
        self.owner.finalization_calls.fetch_add(1, Ordering::AcqRel);
        if self.owner.finalization_interrupted.load(Ordering::Acquire) {
            CleanupProgress::Interrupted(RemovalStage::LocalCleanup)
        } else if self.owner.finalization_ready.load(Ordering::Acquire) {
            CleanupProgress::Verified
        } else {
            CleanupProgress::Blocked(RemovalStage::LocalCleanup)
        }
    }
    fn retry_after_finalization(
        &mut self,
        finalized: &crate::profile_removal::FinalizedRemoval,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        deadline: std::time::Instant,
    ) -> crate::profile_removal::CleanupProgress {
        use crate::profile_removal::{CleanupProgress, RemovalStage};
        use std::sync::atomic::Ordering;
        assert!(finalized.attempt().same_attempt(retry.attempt()));
        self.owner
            .finalization_retry_calls
            .fetch_add(1, Ordering::AcqRel);
        if !self.owner.recovery.load(Ordering::Acquire) {
            return CleanupProgress::Interrupted(RemovalStage::LocalCleanup);
        }
        self.owner
            .finalization_interrupted
            .store(false, Ordering::Release);
        self.acknowledge_finalization(finalized, deadline)
    }
    fn authorization_status(
        &mut self,
        attempt: &crate::profile_removal::RemovalAttempt,
        _: std::time::Instant,
    ) -> crate::profile_removal::PreparedStatus {
        use crate::profile_removal::{PreparedStatus, RemovalStage};
        assert!(attempt.same_attempt(&self.attempt));
        if self
            .owner
            .authorization_interrupted
            .load(std::sync::atomic::Ordering::Acquire)
        {
            PreparedStatus::Interrupted(RemovalStage::NativeSettlement)
        } else if self
            .owner
            .authorization_pending
            .load(std::sync::atomic::Ordering::Acquire)
        {
            PreparedStatus::Pending(RemovalStage::LocalCleanup)
        } else {
            PreparedStatus::Current
        }
    }
    fn retry_before_authorization(
        &mut self,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        deadline: std::time::Instant,
    ) -> crate::profile_removal::PreparedStatus {
        use std::sync::atomic::Ordering;
        assert!(retry.attempt().same_attempt(&self.attempt));
        self.owner
            .authorization_retry_calls
            .fetch_add(1, Ordering::AcqRel);
        if self.owner.recovery.load(Ordering::Acquire) {
            self.owner
                .authorization_interrupted
                .store(false, Ordering::Release);
        }
        self.authorization_status(retry.attempt(), deadline)
    }
    fn retry_cleanup_after_authorization(
        &mut self,
        authorization: &crate::profile_removal::AuthorizedRemoval,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        deadline: std::time::Instant,
    ) -> crate::profile_removal::CleanupProgress {
        use crate::profile_removal::{CleanupProgress, RemovalStage};
        use std::sync::atomic::Ordering;
        assert!(authorization.attempt().same_attempt(&self.attempt));
        assert!(retry.attempt().same_attempt(&self.attempt));
        self.owner
            .cleanup_retry_calls
            .fetch_add(1, Ordering::AcqRel);
        if !self.owner.recovery.load(Ordering::Acquire) {
            return CleanupProgress::Interrupted(RemovalStage::LocalCleanup);
        }
        self.owner
            .cleanup_interrupted
            .store(false, Ordering::Release);
        self.cleanup_after_authorization(authorization, deadline)
    }
    fn attempt(&self) -> &crate::profile_removal::RemovalAttempt {
        &self.attempt
    }
    fn kind(&self) -> crate::profile_removal::PreparedKind {
        crate::profile_removal::PreparedKind::SignedOutSettled
    }
    fn cleanup_after_authorization(
        &mut self,
        authorization: &crate::profile_removal::AuthorizedRemoval,
        _: std::time::Instant,
    ) -> crate::profile_removal::CleanupProgress {
        use crate::profile_removal::{CleanupProgress, RemovalStage};
        assert!(authorization.attempt().same_attempt(&self.attempt));
        assert!(self
            .owner
            .store
            .pending_deletions
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.profile == self.attempt.profile()));
        self.owner
            .store
            .events
            .lock()
            .unwrap()
            .push("native-cleanup");
        if self
            .owner
            .cleanup_interrupted
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return CleanupProgress::Interrupted(RemovalStage::LocalCleanup);
        }
        if self
            .owner
            .cleaned
            .load(std::sync::atomic::Ordering::Acquire)
        {
            CleanupProgress::Verified
        } else {
            CleanupProgress::Blocked(RemovalStage::LocalCleanup)
        }
    }
}
struct NativeRecovered {
    owner: Arc<NativeRemovalFixture>,
    profile: ProfileId,
    activated: bool,
}
impl crate::profile_removal::RecoverableRemoval for NativeRecovered {
    fn stage(&self) -> crate::profile_removal::RecoveredRemovalStage {
        if self
            .owner
            .completed_records
            .lock()
            .unwrap()
            .contains(&self.profile)
        {
            crate::profile_removal::RecoveredRemovalStage::FinalizationPending
        } else {
            crate::profile_removal::RecoveredRemovalStage::Pending
        }
    }
    fn retry_finalization(
        &mut self,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        deadline: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        assert_eq!(
            self.stage(),
            crate::profile_removal::RecoveredRemovalStage::FinalizationPending
        );
        self.activated = true;
        self.owner
            .finalization_retry_calls
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.resume_finalization(
            &crate::profile_removal::FinalizedRemoval::new(retry.attempt().clone()),
            deadline,
        )
    }
    fn resume_finalization(
        &mut self,
        finalized: &crate::profile_removal::FinalizedRemoval,
        _: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        use crate::profile_removal::{Preparation, RemovalStage};
        assert_eq!(finalized.profile(), self.profile);
        assert!(!self
            .owner
            .store
            .pending_deletions
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.profile == self.profile));
        if !self.activated
            || !self
                .owner
                .recovery
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Preparation::Interrupted(RemovalStage::RecoveryRequired);
        }
        Preparation::Prepared(Box::new(NativePrepared {
            owner: self.owner.clone(),
            attempt: finalized.attempt().clone(),
        }))
    }
    fn retry_authorized(
        &mut self,
        authorization: &crate::profile_removal::AuthorizedRemoval,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        deadline: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        assert!(authorization.attempt().same_attempt(retry.attempt()));
        self.resume_authorized(authorization, deadline)
    }
    fn resume_authorized(
        &mut self,
        authorization: &crate::profile_removal::AuthorizedRemoval,
        _: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        use crate::profile_removal::{Preparation, RemovalStage};
        assert_eq!(authorization.profile(), self.profile);
        assert!(self
            .owner
            .store
            .pending_deletions
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.profile == self.profile));
        if !self
            .owner
            .recovery
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Preparation::Blocked(RemovalStage::RecoveryRequired);
        }
        Preparation::Prepared(Box::new(NativePrepared {
            owner: self.owner.clone(),
            attempt: authorization.attempt().clone(),
        }))
    }
    fn profile(&self) -> ProfileId {
        self.profile
    }
    fn retry(
        &mut self,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        deadline: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        self.activated = true;
        self.reconcile(retry.attempt(), deadline)
    }
    fn reconcile(
        &mut self,
        attempt: &crate::profile_removal::RemovalAttempt,
        _: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        use crate::profile_removal::{Preparation, RemovalStage};
        assert!(self.activated);
        assert_eq!(attempt.profile(), self.profile);
        if !self
            .owner
            .recovery
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Preparation::Interrupted(RemovalStage::RecoveryRequired);
        }
        Preparation::Prepared(Box::new(NativePrepared {
            owner: self.owner.clone(),
            attempt: attempt.clone(),
        }))
    }
}
impl crate::profile_removal::ProfileRemovalParticipant for NativeRemovalPort {
    fn recheck_inventory(
        &self,
        _: &crate::profile_removal::InventoryRecheck,
        deadline: std::time::Instant,
    ) -> crate::profile_removal::RecoveryInventory {
        self.recover_pending(deadline)
    }

    fn retry_authorized(
        &self,
        authorization: &crate::profile_removal::AuthorizedRemoval,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        _: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        use crate::profile_removal::{Preparation, RemovalStage};
        assert!(authorization.attempt().same_attempt(retry.attempt()));
        self.0
            .retry_calls
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if !self.0.recovery.load(std::sync::atomic::Ordering::Acquire) {
            return Preparation::Interrupted(RemovalStage::RecoveryRequired);
        }
        Preparation::Prepared(Box::new(NativePrepared {
            owner: self.0.clone(),
            attempt: retry.attempt().clone(),
        }))
    }
    fn seal_inventory(&self) {
        self.0
            .sealed
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    fn recover_pending(&self, _: std::time::Instant) -> crate::profile_removal::RecoveryInventory {
        crate::profile_removal::RecoveryInventory {
            status: if self
                .0
                .inventory_available
                .load(std::sync::atomic::Ordering::Acquire)
            {
                crate::profile_removal::InventoryStatus::Verified
            } else {
                crate::profile_removal::InventoryStatus::Unavailable
            },
            records: self
                .0
                .records
                .lock()
                .unwrap()
                .iter()
                .chain(self.0.completed_records.lock().unwrap().iter())
                .map(|profile| {
                    Box::new(NativeRecovered {
                        owner: self.0.clone(),
                        profile: *profile,
                        activated: false,
                    }) as Box<dyn crate::profile_removal::RecoverableRemoval>
                })
                .collect(),
        }
    }
    fn retry(
        &self,
        retry: &crate::profile_removal::ExplicitRemovalRetry,
        _: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        use crate::profile_removal::{Preparation, RemovalStage};
        use std::sync::atomic::Ordering;
        self.0.retry_calls.fetch_add(1, Ordering::AcqRel);
        assert!(self
            .0
            .attempt
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .same_attempt(retry.attempt()));
        // Reconciliation of separately verified protected Prepared facts, not
        // resetting the permanently interrupted original request.
        if !self.0.recovery.load(Ordering::Acquire) {
            return Preparation::Interrupted(RemovalStage::RecoveryRequired);
        }
        Preparation::Prepared(Box::new(NativePrepared {
            owner: self.0.clone(),
            attempt: retry.attempt().clone(),
        }))
    }
    fn prepare(
        &self,
        attempt: &crate::profile_removal::RemovalAttempt,
        _: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        use crate::profile_removal::{Preparation, RemovalAttempt, RemovalStage};
        use std::sync::atomic::Ordering;
        assert!(self.0.store.pending_deletions.lock().unwrap().is_empty());
        self.0.prepare_calls.fetch_add(1, Ordering::AcqRel);
        if self.0.busy.load(Ordering::Acquire) {
            return Preparation::Busy;
        }
        self.0.store.events.lock().unwrap().push("native-prepare");
        let mut prior = self.0.attempt.lock().unwrap();
        if let Some(prior) = prior.as_ref() {
            assert!(prior.same_attempt(attempt));
        } else {
            *prior = Some(attempt.clone());
        }
        if self.0.interrupted.load(Ordering::Acquire) {
            return Preparation::Interrupted(RemovalStage::RecoveryRequired);
        }
        if self.0.blocked.load(Ordering::Acquire) {
            return Preparation::Blocked(RemovalStage::RecoveryRequired);
        }
        if !self.0.prepared.load(Ordering::Acquire) {
            return Preparation::Pending(RemovalStage::LogoutPending);
        }
        Preparation::Prepared(Box::new(NativePrepared {
            owner: self.0.clone(),
            attempt: if self.0.wrong_attempt.load(Ordering::Acquire) {
                RemovalAttempt::new(attempt.profile())
            } else {
                attempt.clone()
            },
        }))
    }
    fn resume_authorized(
        &self,
        authorization: &crate::profile_removal::AuthorizedRemoval,
        _: std::time::Instant,
    ) -> crate::profile_removal::Preparation {
        use crate::profile_removal::{Preparation, RemovalStage};
        use std::sync::atomic::Ordering;
        self.0.recovery_calls.fetch_add(1, Ordering::AcqRel);
        self.0.store.events.lock().unwrap().push("native-recovery");
        assert!(self
            .0
            .store
            .pending_deletions
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.profile == authorization.profile()));
        if self.0.interrupted.load(Ordering::Acquire) {
            return Preparation::Interrupted(RemovalStage::RecoveryRequired);
        }
        if !self.0.recovery.load(Ordering::Acquire) {
            return Preparation::Blocked(RemovalStage::RecoveryRequired);
        }
        Preparation::Prepared(Box::new(NativePrepared {
            owner: self.0.clone(),
            attempt: authorization.attempt().clone(),
        }))
    }
}

#[test]
fn native_removal_precedes_store_authorization_and_cleanup_precedes_engine_erase() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_100);
    let native = NativeRemovalFixture::new(store.clone());
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    store.events.lock().unwrap().clear();
    shell.handle(delete_operation("native-removal", profile));
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert!(engine.erasure_requests().is_empty());
    assert!(operations.lock().unwrap().is_empty());
    assert!(shell.profile_deletion_quarantines(profile));
    let view = shell.profiles_view().unwrap();
    assert!(!view
        .profiles
        .iter()
        .any(|row| row.id == profile.to_string()));
    assert_eq!(
        view.removals[0].native,
        zephium_ipc::ProfileRemovalNativeV1::Unconfirmed
    );
    assert_eq!(
        view.removals[0].cleanup,
        zephium_ipc::ProfileRemovalCleanupV1::NotStarted
    );
    native.prepared.store(true, Ordering::Release);
    shell.drive_profile_deletion(profile);
    assert_eq!(store.authorized_sessions.lock().unwrap().len(), 1);
    assert!(engine.erasure_requests().is_empty());
    assert_eq!(
        shell.profile_deletion.states[&profile].native_progress,
        Some((crate::profile_removal::RemovalStage::LocalCleanup, true))
    );
    assert!(!store.events.lock().unwrap().contains(&"finalize-delete"));
    let progress = shell.profiles_view().unwrap().removals.remove(0);
    assert_eq!(
        progress.native,
        zephium_ipc::ProfileRemovalNativeV1::SignedOutSettled
    );
    assert_eq!(
        progress.cleanup,
        zephium_ipc::ProfileRemovalCleanupV1::Unconfirmed
    );
    assert!(progress.blocked);
    native.cleaned.store(true, Ordering::Release);
    shell.drive_profile_deletion(profile);
    assert_eq!(engine.erasure_requests(), vec![profile]);
    assert_eq!(operations.lock().unwrap().len(), 1);
    assert_eq!(
        operations.lock().unwrap()[0].reason,
        OperationReason::ProfileDeletionCompleted
    );
    assert_eq!(
        store.events.lock().unwrap().as_slice(),
        [
            "native-prepare",
            "native-prepare",
            "authorize-delete",
            "native-cleanup",
            "native-cleanup",
            "finalize-delete"
        ]
    );
    assert_eq!(native.recovery_calls.load(Ordering::Acquire), 0);
}

#[test]
fn uncertain_store_authorization_retains_exact_prepared_native_attempt() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::OutcomeUnknown);
    store
        .authorize_unknown_commits
        .store(true, Ordering::Release);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_200);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    native.cleaned.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    shell.handle(delete_operation("native-uncertain-auth", profile));
    shell.drive_profile_deletion(profile);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(native.recovery_calls.load(Ordering::Acquire), 0);
    assert_eq!(operations.lock().unwrap().len(), 1);
    assert_eq!(engine.erasure_requests(), vec![profile]);
}

#[test]
fn failed_store_authorization_rechecks_current_prepared_before_any_new_handoff() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::{ProfileRemovalPhaseV1, ProfileRemovalRetryV1};
    let store = Arc::new(FakeStore::default());
    store.authorize_outcomes.lock().unwrap().extend([
        ProfileDeletionAuthorizeOutcome::Failed,
        ProfileDeletionAuthorizeOutcome::Authorized,
    ]);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_201);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    native.cleaned.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    shell.handle(delete_operation("current-prepared", profile));
    let handoffs = || {
        store
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == "authorize-delete")
            .count()
    };
    assert_eq!(handoffs(), 1);
    native
        .authorization_interrupted
        .store(true, Ordering::Release);
    shell.drive_profile_deletion(profile);
    assert_eq!(handoffs(), 1);
    assert_eq!(
        shell.profiles_view().unwrap().removals[0].phase,
        ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    assert_eq!(
        shell.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Blocked
    );
    assert_eq!(handoffs(), 1);
    assert!(engine.erasure_requests().is_empty());
    native.recovery.store(true, Ordering::Release); // exact protected Prepared/local retry join
    assert_eq!(
        shell.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Scheduled
    );
    assert_eq!(handoffs(), 2);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(native.retry_calls.load(Ordering::Acquire), 0);
    assert_eq!(native.recovery_calls.load(Ordering::Acquire), 0);
    assert_eq!(native.authorization_retry_calls.load(Ordering::Acquire), 2);
    assert_eq!(engine.erasure_requests(), vec![profile]);
    assert_eq!(operations.lock().unwrap().len(), 1);
}

#[test]
fn interrupted_action_does_not_hide_already_committed_unknown_authorization() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::{ProfileRemovalPhaseV1, ProfileRemovalRetryV1};
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::OutcomeUnknown);
    store
        .authorize_unknown_commits
        .store(true, Ordering::Release);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_202);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    shell.handle(delete_operation("unknown-then-cancelled", profile));
    native
        .authorization_interrupted
        .store(true, Ordering::Release);
    native.cleanup_interrupted.store(true, Ordering::Release);
    shell.drive_profile_deletion(profile);
    assert_eq!(store.pending_deletions.lock().unwrap().len(), 1);
    assert_eq!(
        shell.profiles_view().unwrap().removals[0].phase,
        ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    assert!(engine.erasure_requests().is_empty());
    native.recovery.store(true, Ordering::Release);
    native.cleaned.store(true, Ordering::Release);
    assert_eq!(
        shell.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Scheduled
    );
    assert_eq!(native.authorization_retry_calls.load(Ordering::Acquire), 0);
    assert_eq!(native.cleanup_retry_calls.load(Ordering::Acquire), 1);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        store
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == "authorize-delete")
            .count(),
        1
    );
    assert_eq!(engine.erasure_requests(), vec![profile]);
    assert_eq!(operations.lock().unwrap().len(), 1);
}

#[test]
fn replacement_attempt_receipt_cannot_authorize_deletion() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    let failures = Arc::new(Mutex::new(Vec::new()));
    let reported = failures.clone();
    let (mut shell, engine, _, operations) = setup_with_operation_log_and_failure(
        store.clone(),
        Box::new(move |failure| reported.lock().unwrap().push(failure)),
    );
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_300);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    native.wrong_attempt.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native)));
    shell.handle(delete_operation("native-wrong-attempt", profile));
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert!(engine.erasure_requests().is_empty());
    assert!(operations.lock().unwrap().is_empty());
    assert_eq!(
        *failures.lock().unwrap(),
        vec![ShellTerminalFailure::ProfileDeletionInvariant]
    );
}

#[test]
fn recovered_journal_requires_protected_native_recovery_not_fresh_registered_logout() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut first, first_engine, _, first_operations) = setup_with_operation_log(store.clone());
    first.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut first, 80_400);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    first.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    first.handle(delete_operation("native-before-restart", profile));
    assert!(first_engine.erasure_requests().is_empty());
    assert!(first_operations.lock().unwrap().is_empty());
    drop(first);
    let (mut restarted, engine, _, operations) = setup_with_operation_log(store.clone());
    restarted.profile_deletion.native_participant =
        Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    restarted.handle(Command::Bootstrap);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert!(native.recovery_calls.load(Ordering::Acquire) > 0);
    assert!(engine.erasure_requests().is_empty());
    assert!(operations.lock().unwrap().is_empty());
    assert!(!store.pending_deletions.lock().unwrap().is_empty());
    native.recovery.store(true, Ordering::Release);
    native.cleaned.store(true, Ordering::Release);
    restarted.drive_profile_deletion(profile);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(engine.erasure_requests(), vec![profile]);
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    // A prior process's operation id is never fabricated in this process.
    assert!(operations.lock().unwrap().is_empty());
}

#[test]
fn explicit_retry_reconciles_only_existing_attempt_without_rearming_original_request() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::ProfileRemovalRetryV1;
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_500);
    let native = NativeRemovalFixture::new(store.clone());
    native.interrupted.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    shell.handle(delete_operation("retained-native-op", profile));
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    shell.drive_profile_deletion(profile); // timer cannot reopen an interrupted intent
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        shell.retry_profile_removal(ProfileId::from(999_u128)),
        ProfileRemovalRetryV1::NotPending
    );
    assert_eq!(
        shell.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Blocked
    );
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        shell.profile_deletion.states[&profile]
            .operation_id
            .as_deref(),
        Some("retained-native-op")
    );
    native.recovery.store(true, Ordering::Release); // independently verified Prepared custody
    native.cleaned.store(true, Ordering::Release);
    let (reply, received) = std::sync::mpsc::sync_channel(1);
    shell.handle(Command::RetryProfileRemoval { profile, reply });
    assert_eq!(received.recv().unwrap(), ProfileRemovalRetryV1::Scheduled);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(native.retry_calls.load(Ordering::Acquire), 2);
    assert_eq!(operations.lock().unwrap().len(), 1);
    assert_eq!(
        operations.lock().unwrap()[0].operation_id,
        "retained-native-op"
    );
    assert!(shell.profiles_view().unwrap().removals.is_empty());
    assert_eq!(
        shell.retry_profile_removal(profile),
        ProfileRemovalRetryV1::NotPending
    );
}

#[test]
fn interrupted_authorized_recovery_pauses_then_retries_cleanup_without_active_grant() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::ProfileRemovalRetryV1;
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut first, _, _, _) = setup_with_operation_log(store.clone());
    first.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut first, 80_600);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    first.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    first.handle(delete_operation("interrupted-authorized", profile));
    assert_eq!(store.pending_deletions.lock().unwrap().len(), 1);
    drop(first);
    native.interrupted.store(true, Ordering::Release);
    let (mut restarted, engine, _, operations) = setup_with_operation_log(store.clone());
    restarted.profile_deletion.native_participant =
        Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    restarted.handle(Command::Bootstrap);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    let reads = native.recovery_calls.load(Ordering::Acquire);
    restarted.drive_profile_deletion(profile);
    assert_eq!(native.recovery_calls.load(Ordering::Acquire), reads);
    assert_eq!(
        restarted.profiles_view().unwrap().removals[0].phase,
        zephium_ipc::ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    assert_eq!(
        restarted.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Blocked
    );
    assert!(engine.erasure_requests().is_empty());
    native.recovery.store(true, Ordering::Release); // protected authorized Prepared proof
    native.cleaned.store(true, Ordering::Release);
    assert_eq!(
        restarted.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Scheduled
    );
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(store.authorized_sessions.lock().unwrap().len(), 1);
    assert_eq!(engine.erasure_requests(), vec![profile]);
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    assert!(operations.lock().unwrap().is_empty());
}

#[test]
fn interrupted_held_cleanup_retries_same_prepared_custody_without_provider_recovery() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::{ProfileRemovalPhaseV1, ProfileRemovalRetryV1};
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_601);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    native.cleanup_interrupted.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    shell.handle(delete_operation("held-cleanup-retry", profile));
    assert_eq!(
        shell.profiles_view().unwrap().removals[0].phase,
        ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    let calls = store.events.lock().unwrap().len();
    shell.drive_profile_deletion(profile);
    assert_eq!(store.events.lock().unwrap().len(), calls);
    assert!(engine.erasure_requests().is_empty());
    assert_eq!(
        shell.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Blocked
    );
    assert_eq!(native.cleanup_retry_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        shell.profiles_view().unwrap().removals[0].phase,
        ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    native.recovery.store(true, Ordering::Release); // exact same-custody local retry proof
    native.cleaned.store(true, Ordering::Release);
    assert_eq!(
        shell.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Scheduled
    );
    assert_eq!(native.cleanup_retry_calls.load(Ordering::Acquire), 2);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(native.retry_calls.load(Ordering::Acquire), 0);
    assert_eq!(native.recovery_calls.load(Ordering::Acquire), 0);
    assert_eq!(store.authorized_sessions.lock().unwrap().len(), 1);
    assert_eq!(engine.erasure_requests(), vec![profile]);
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    let completions = operations.lock().unwrap();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].operation_id, "held-cleanup-retry");
}

#[test]
fn protected_preauthorization_inventory_quarantines_before_views_and_requires_explicit_retry() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    let (mut first, _, _) = setup_with(store.clone());
    first.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut first, 80_700);
    first.handle(Command::Persist);
    let target_space = first.spaces.first_for(profile).unwrap();
    store.saved.lock().unwrap().as_mut().unwrap().active_space = Some(target_space);
    drop(first);
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let native = NativeRemovalFixture::new(store.clone());
    native.records.lock().unwrap().push(profile); // synthetic verified protected intent
    native.recovery.store(true, Ordering::Release);
    native.cleaned.store(true, Ordering::Release);
    let (mut restored, engine, _, operations) = setup_with_operation_log(store.clone());
    restored.profile_deletion.native_participant =
        Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    restored.handle(Command::Bootstrap);
    assert!(restored.bootstrapped);
    assert_ne!(restored.windows.focused().unwrap().profile, profile);
    assert!(restored.profile_deletion_quarantines(profile));
    assert_eq!(
        restored.profiles_view().unwrap().removals[0].phase,
        zephium_ipc::ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert!(engine.erasure_requests().is_empty());
    restored.drive_profile_deletion(profile);
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 0);
    assert_eq!(
        restored.retry_profile_removal(profile),
        zephium_ipc::ProfileRemovalRetryV1::Scheduled
    );
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 0);
    assert_eq!(engine.erasure_requests(), vec![profile]);
    assert!(operations.lock().unwrap().is_empty());
}

#[test]
fn unavailable_native_inventory_seals_native_integration_without_bricking_the_browser() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    let native = NativeRemovalFixture::new(store.clone());
    native.inventory_available.store(false, Ordering::Release);
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    shell.handle(Command::Bootstrap);
    assert!(shell.bootstrapped);
    assert!(shell.windows.focused().is_some());
    assert_eq!(native.sealed.load(Ordering::Acquire), 1);
    assert_eq!(
        shell.profile_deletion.native_inventory,
        Some(crate::profile_removal::InventoryStatus::Unavailable)
    );
    assert!(shell.profiles_view().unwrap().removals.is_empty()); // no fabricated profile id
    assert_eq!(
        shell.profiles_view().unwrap().removal_recovery,
        Some(zephium_ipc::ProfileRemovalRecoveryV1::Unavailable)
    );
    let profile = add_inactive_named_profile(&mut shell, 80_800);
    shell.handle(delete_operation("unprovable-native-inventory", profile));
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert!(engine.erasure_requests().is_empty());
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 0);
    assert_eq!(
        operations.lock().unwrap().last().unwrap().outcome,
        OperationOutcome::Rejected
    );
}

#[test]
fn malformed_native_inventory_seals_admission_and_preserves_known_quarantine_only() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    let (mut first, _, _) = setup_with(store.clone());
    first.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut first, 80_900);
    let default = first.profiles.default_profile().unwrap();
    first.handle(Command::Persist);
    drop(first);
    let native = NativeRemovalFixture::new(store.clone());
    native
        .records
        .lock()
        .unwrap()
        .extend([profile, profile, default]);
    let (mut restored, engine, _) = setup_with(store.clone());
    restored.profile_deletion.native_participant =
        Some(Arc::new(NativeRemovalPort(native.clone())));
    restored.handle(Command::Bootstrap);
    assert!(restored.bootstrapped);
    assert_eq!(native.sealed.load(Ordering::Acquire), 1);
    assert!(restored.profile_deletion_quarantines(profile));
    assert!(!restored.profile_deletion_quarantines(default));
    let progress = restored.profiles_view().unwrap().removals;
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].profile, profile.to_string());
    assert_eq!(
        restored.retry_profile_removal(profile),
        zephium_ipc::ProfileRemovalRetryV1::Blocked
    );
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert!(engine.erasure_requests().is_empty());
}

#[test]
fn explicit_inventory_recheck_observes_existing_intent_without_authorizing_or_replacing_it() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::ProfileRemovalRetryV1;
    let store = Arc::new(FakeStore::default());
    let (mut first, _, _) = setup_with(store.clone());
    first.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut first, 81_000);
    first.handle(Command::Persist);
    drop(first);
    let native = NativeRemovalFixture::new(store.clone());
    native.inventory_available.store(false, Ordering::Release);
    native.records.lock().unwrap().push(profile);
    let (mut restored, engine, _) = setup_with(store.clone());
    restored.profile_deletion.native_participant =
        Some(Arc::new(NativeRemovalPort(native.clone())));
    restored.handle(Command::Bootstrap);
    let attempt = restored.profile_deletion.states[&profile]
        .native_attempt
        .clone()
        .unwrap();
    assert_eq!(
        restored.recheck_profile_removal_inventory(),
        ProfileRemovalRetryV1::Blocked
    );
    native.inventory_available.store(true, Ordering::Release);
    let (reply, received) = std::sync::mpsc::sync_channel(1);
    restored.handle(Command::RecheckProfileRemovalInventory { reply });
    assert_eq!(received.recv().unwrap(), ProfileRemovalRetryV1::Scheduled);
    assert_eq!(
        restored.profiles_view().unwrap().removal_recovery,
        Some(zephium_ipc::ProfileRemovalRecoveryV1::Verified)
    );
    assert!(attempt.same_attempt(
        restored.profile_deletion.states[&profile]
            .native_attempt
            .as_ref()
            .unwrap()
    ));
    assert_eq!(
        restored.profiles_view().unwrap().removals[0].phase,
        zephium_ipc::ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    assert!(store.authorized_sessions.lock().unwrap().is_empty());
    assert!(engine.erasure_requests().is_empty());
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 0);
    assert_eq!(native.retry_calls.load(Ordering::Acquire), 0);
}

#[test]
fn canonical_finalization_keeps_native_receipt_pending_and_never_repeats_erasure() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::{ProfileRemovalPhaseV1, ProfileRemovalRetryV1};
    for unknown_ack in [false, true] {
        let store = Arc::new(FakeStore::default());
        store
            .authorize_outcomes
            .lock()
            .unwrap()
            .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
        store
            .finalize_outcomes
            .lock()
            .unwrap()
            .push_back(if unknown_ack {
                ProfileDeletionFinalizeOutcome::OutcomeUnknown
            } else {
                ProfileDeletionFinalizeOutcome::Completed
            });
        store
            .finalize_unknown_completes
            .store(unknown_ack, Ordering::Release);
        let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
        shell.handle(Command::Bootstrap);
        let profile = add_inactive_named_profile(&mut shell, 80_810 + u128::from(unknown_ack));
        let native = NativeRemovalFixture::new(store.clone());
        native.prepared.store(true, Ordering::Release);
        native.cleaned.store(true, Ordering::Release);
        native.finalization_ready.store(false, Ordering::Release);
        shell.profile_deletion.native_participant =
            Some(Arc::new(NativeRemovalPort(native.clone())));
        engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
        shell.handle(delete_operation("final-native-receipt", profile));
        shell.drive_profile_deletion(profile);
        assert!(store.pending_deletions.lock().unwrap().is_empty());
        assert!(operations.lock().unwrap().is_empty());
        assert_eq!(
            shell.profiles_view().unwrap().removals[0].phase,
            ProfileRemovalPhaseV1::AcknowledgingNative
        );
        let store_events = store.events.lock().unwrap().clone();
        shell.drive_profile_deletion(profile);
        assert_eq!(*store.events.lock().unwrap(), store_events);
        assert_eq!(engine.erasure_requests(), vec![profile]);
        native
            .finalization_interrupted
            .store(true, Ordering::Release);
        shell.drive_profile_deletion(profile);
        let calls = native.finalization_calls.load(Ordering::Acquire);
        shell.drive_profile_deletion(profile);
        assert_eq!(native.finalization_calls.load(Ordering::Acquire), calls);
        assert_eq!(
            shell.profiles_view().unwrap().removals[0].phase,
            ProfileRemovalPhaseV1::AwaitingExplicitRetry
        );
        assert_eq!(
            shell.retry_profile_removal(profile),
            ProfileRemovalRetryV1::Blocked
        );
        native.recovery.store(true, Ordering::Release);
        native.finalization_ready.store(true, Ordering::Release);
        assert_eq!(
            shell.retry_profile_removal(profile),
            ProfileRemovalRetryV1::Scheduled
        );
        assert_eq!(*store.events.lock().unwrap(), store_events);
        assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
        assert_eq!(native.retry_calls.load(Ordering::Acquire), 0);
        assert_eq!(native.finalization_retry_calls.load(Ordering::Acquire), 2);
        assert_eq!(operations.lock().unwrap().len(), 1);
        assert_eq!(
            operations.lock().unwrap()[0].operation_id,
            "final-native-receipt"
        );
    }
}

#[test]
fn recovered_completed_receipt_is_visible_and_retry_only_acknowledges_finalization() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::{ProfileRemovalPhaseV1, ProfileRemovalRetryV1};
    let store = Arc::new(FakeStore::default());
    store
        .authorize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
    store
        .finalize_outcomes
        .lock()
        .unwrap()
        .push_back(ProfileDeletionFinalizeOutcome::Completed);
    let (mut first, engine, _, _) = setup_with_operation_log(store.clone());
    first.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut first, 80_812);
    let native = NativeRemovalFixture::new(store.clone());
    native.prepared.store(true, Ordering::Release);
    native.cleaned.store(true, Ordering::Release);
    native.finalization_ready.store(false, Ordering::Release);
    first.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
    first.handle(delete_operation("completed-restart", profile));
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    native.completed_records.lock().unwrap().push(profile); // protected receipt plus canonical finalized join
    drop(first);
    let (mut restored, engine, _, operations) = setup_with_operation_log(store.clone());
    restored.profile_deletion.native_participant =
        Some(Arc::new(NativeRemovalPort(native.clone())));
    restored.handle(Command::Bootstrap);
    let view = restored.profiles_view().unwrap();
    assert!(!view
        .profiles
        .iter()
        .any(|row| row.id == profile.to_string()));
    assert_eq!(view.removals.len(), 1);
    assert_eq!(
        view.removals[0].phase,
        ProfileRemovalPhaseV1::AwaitingExplicitRetry
    );
    let store_events = store.events.lock().unwrap().clone();
    let calls = native.finalization_calls.load(Ordering::Acquire);
    restored.drive_profile_deletion(profile);
    assert_eq!(native.finalization_calls.load(Ordering::Acquire), calls);
    native.recovery.store(true, Ordering::Release);
    native.finalization_ready.store(true, Ordering::Release);
    assert_eq!(
        restored.retry_profile_removal(profile),
        ProfileRemovalRetryV1::Scheduled
    );
    assert_eq!(*store.events.lock().unwrap(), store_events);
    assert!(engine.erasure_requests().is_empty());
    assert!(operations.lock().unwrap().is_empty());
    assert_eq!(native.finalization_retry_calls.load(Ordering::Acquire), 1);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
    assert_eq!(native.retry_calls.load(Ordering::Acquire), 0);
    assert!(restored.profiles_view().unwrap().removals.is_empty());
}

#[test]
fn completed_inventory_cannot_acknowledge_an_active_registered_profile() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    let (mut shell, engine, _) = setup_with(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_813);
    let native = NativeRemovalFixture::new(store.clone());
    native.completed_records.lock().unwrap().push(profile);
    native.recovery.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    shell.recover_native_profile_removals(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    );
    assert_eq!(
        shell.profile_removal_recovery_view(),
        Some(zephium_ipc::ProfileRemovalRecoveryV1::Unavailable)
    );
    assert!(shell.profiles_view().unwrap().removals.is_empty());
    assert!(engine.erasure_requests().is_empty());
    assert_eq!(native.finalization_calls.load(Ordering::Acquire), 0);
    assert_eq!(native.finalization_retry_calls.load(Ordering::Acquire), 0);
}

#[test]
fn pending_same_object_renewal_never_authorizes_until_current_or_reissues_retry() {
    use std::sync::atomic::Ordering;
    use zephium_ipc::{ProfileRemovalPhaseV1, ProfileRemovalRetryV1};
    for revoke in [false, true] {
        let store = Arc::new(FakeStore::default());
        store
            .authorize_outcomes
            .lock()
            .unwrap()
            .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
        store
            .finalize_outcomes
            .lock()
            .unwrap()
            .push_back(ProfileDeletionFinalizeOutcome::Completed);
        let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
        shell.handle(Command::Bootstrap);
        let profile = add_inactive_named_profile(&mut shell, 80_830 + u128::from(revoke));
        let native = NativeRemovalFixture::new(store.clone());
        native.prepared.store(true, Ordering::Release);
        native.cleaned.store(true, Ordering::Release);
        native
            .authorization_interrupted
            .store(true, Ordering::Release);
        shell.profile_deletion.native_participant =
            Some(Arc::new(NativeRemovalPort(native.clone())));
        engine.push_erasure_outcomes([ProfileDataErasureOutcome::Verified]);
        shell.handle(delete_operation("pending-local-renewal", profile));
        assert!(store.authorized_sessions.lock().unwrap().is_empty());
        native.recovery.store(true, Ordering::Release);
        native.authorization_pending.store(true, Ordering::Release);
        assert_eq!(
            shell.retry_profile_removal(profile),
            ProfileRemovalRetryV1::Scheduled
        );
        assert_eq!(native.authorization_retry_calls.load(Ordering::Acquire), 1);
        for _ in 0..3 {
            shell.drive_profile_deletion(profile);
        }
        assert_eq!(native.authorization_retry_calls.load(Ordering::Acquire), 1);
        assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
        assert!(store.authorized_sessions.lock().unwrap().is_empty());
        assert!(engine.erasure_requests().is_empty());
        assert!(operations.lock().unwrap().is_empty());
        if revoke {
            native
                .authorization_interrupted
                .store(true, Ordering::Release);
        }
        native.authorization_pending.store(false, Ordering::Release);
        shell.drive_profile_deletion(profile);
        if revoke {
            assert!(store.authorized_sessions.lock().unwrap().is_empty());
            assert!(operations.lock().unwrap().is_empty());
            assert_eq!(
                shell.profiles_view().unwrap().removals[0].phase,
                ProfileRemovalPhaseV1::AwaitingExplicitRetry
            );
        } else {
            assert_eq!(store.authorized_sessions.lock().unwrap().len(), 1);
            assert_eq!(engine.erasure_requests(), vec![profile]);
            assert_eq!(operations.lock().unwrap().len(), 1);
        }
        assert_eq!(native.authorization_retry_calls.load(Ordering::Acquire), 1);
    }
}

#[test]
fn native_busy_initial_admission_keeps_profile_and_history_unquarantined_until_explicit_delete() {
    use std::sync::atomic::Ordering;
    let store = Arc::new(FakeStore::default());
    let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
    shell.handle(Command::Bootstrap);
    let profile = add_inactive_named_profile(&mut shell, 80_840);
    let native = NativeRemovalFixture::new(store.clone());
    native.busy.store(true, Ordering::Release);
    shell.profile_deletion.native_participant = Some(Arc::new(NativeRemovalPort(native.clone())));
    let before = shell.profiles_view().unwrap();
    store.events.lock().unwrap().clear();
    shell.handle(delete_operation("native-busy", profile));
    assert!(shell.profile_deletion.states.is_empty());
    assert!(!shell.profile_deletion_quarantines(profile));
    assert_eq!(shell.profiles_view().unwrap(), before);
    assert!(native.attempt.lock().unwrap().is_none());
    assert!(store.events.lock().unwrap().is_empty());
    assert!(store.pending_deletions.lock().unwrap().is_empty());
    assert!(engine.erasure_requests().is_empty());
    assert_eq!(operations.lock().unwrap().len(), 1);
    assert_eq!(
        operations.lock().unwrap()[0].outcome,
        OperationOutcome::Rejected
    );
    assert_eq!(
        operations.lock().unwrap()[0].reason,
        OperationReason::ProfileDeletionInProgress
    );
    shell.drive_profile_deletion(profile);
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1); // no automatic admission retry
    native.busy.store(false, Ordering::Release);
    shell.handle(delete_operation("native-explicit-retry", profile));
    assert!(shell.profile_deletion_quarantines(profile));
    assert_eq!(native.prepare_calls.load(Ordering::Acquire), 2);
    assert_eq!(
        shell.profile_deletion.states[&profile]
            .operation_id
            .as_deref(),
        Some("native-explicit-retry")
    );
}

#[test]
fn initial_native_acceptance_is_retained_and_consumed_once_for_every_result() {
    use std::sync::atomic::Ordering;
    for case in 0..4 {
        let store = Arc::new(FakeStore::default());
        store
            .authorize_outcomes
            .lock()
            .unwrap()
            .push_back(ProfileDeletionAuthorizeOutcome::Authorized);
        let (mut shell, engine, _, operations) = setup_with_operation_log(store.clone());
        shell.handle(Command::Bootstrap);
        let profile = add_inactive_named_profile(&mut shell, 80_850 + case);
        let native = NativeRemovalFixture::new(store.clone());
        native.interrupted.store(case == 1, Ordering::Release);
        native.blocked.store(case == 2, Ordering::Release);
        native.prepared.store(case == 3, Ordering::Release);
        shell.profile_deletion.native_participant =
            Some(Arc::new(NativeRemovalPort(native.clone())));
        shell.handle(delete_operation("initial-native-acceptance", profile));
        assert_eq!(native.prepare_calls.load(Ordering::Acquire), 1);
        let state = &shell.profile_deletion.states[&profile];
        assert!(state
            .native_attempt
            .as_ref()
            .unwrap()
            .same_attempt(native.attempt.lock().unwrap().as_ref().unwrap()));
        assert!(shell.profile_deletion_quarantines(profile));
        assert_eq!(
            state.operation_id.as_deref(),
            Some("initial-native-acceptance")
        );
        assert!(engine.erasure_requests().is_empty());
        assert!(operations.lock().unwrap().is_empty());
        if case == 3 {
            assert_eq!(store.authorized_sessions.lock().unwrap().len(), 1);
        }
    }
}
