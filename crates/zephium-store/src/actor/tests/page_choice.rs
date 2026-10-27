use super::*;
use zephium_core::page_choice::{
    Backend, Call, CodexChoice, CodexEffortChoice, Fault, Reply, Selection,
};
#[test]
fn page_choice_actor_commits_complete_cohort_and_closes_generic_page_writes() {
    let store = SqliteStore::in_memory().unwrap();
    store.save_session(sample());
    let profile = ProfileId::from(1);
    let Reply::Loaded(before) = store.page_choice_call(profile, Call::Read) else {
        panic!()
    };
    let choice = Selection::Codex(CodexChoice {
        model: "original-model".into(),
        effort: CodexEffortChoice::Default,
    });
    let expected = before.updated(choice.clone()).unwrap();
    assert_eq!(
        store.page_choice_call(
            profile,
            Call::CompareAndSwap {
                expected: Box::new(before),
                selection: choice
            }
        ),
        Reply::Committed(expected.clone())
    );
    assert_eq!(
        store.page_choice_call(profile, Call::Read),
        Reply::Loaded(expected)
    );
    for key in zephium_core::page_choice::keys(profile) {
        assert!(!store.set_app_setting(key, "forbidden".into()));
    }
    assert!(store.set_app_setting(
        format!("work.models.{profile}.lead"),
        "unchanged-route".into()
    ));
    assert!(store.flush());
}
#[test]
fn lost_choice_ack_is_unknown_and_prequeue_refusal_is_not_commit() {
    let (tx, rx) = mpsc::sync_channel(1);
    let store = test_store_with_sender(tx);
    let profile = ProfileId::from(1);
    let worker = std::thread::spawn(move || {
        let Cmd::PageChoice(_, _, reply) = rx.recv().unwrap() else {
            panic!()
        };
        drop(reply);
    });
    let before = zephium_core::page_choice::Snapshot {
        profile,
        revision: 0,
        backend: Backend::Model,
        model: None,
        codex: None,
    };
    assert_eq!(
        store.page_choice_call(
            profile,
            Call::CompareAndSwap {
                expected: Box::new(before),
                selection: Selection::Model(None)
            }
        ),
        Reply::Failed(Fault::OutcomeUnknown)
    );
    worker.join().unwrap();
    assert_eq!(
        store.page_choice_call(profile, Call::Read),
        Reply::Failed(Fault::Unavailable)
    );
}
#[test]
fn retained_failed_legacy_setting_is_not_overtaken_by_canonical_read_or_commit() {
    let mut hub = Hub::in_memory().unwrap();
    hub.save(&sample()).unwrap();
    hub.fail_setting_writes();
    let store = SqliteStore::spawn(hub).unwrap();
    let profile = ProfileId::from(1);
    let key = zephium_core::page_choice::keys(profile)[1].clone();
    {
        let mut pending = store.pending_settings.lock().unwrap();
        pending.known_keys.insert(key.clone());
        pending.pending.insert(key.clone(), "codex".into());
    }
    assert_eq!(
        store.page_choice_call(profile, Call::Read),
        Reply::Failed(Fault::Unavailable)
    );
    assert_eq!(
        store
            .pending_settings
            .lock()
            .unwrap()
            .pending
            .get(&key)
            .map(String::as_str),
        Some("codex")
    );
}

#[test]
fn windows_profile_retirement_blocks_same_id_revival_and_stale_choice_cas() {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileId::from(1);
    let mut hub = Hub::open(dir.path().to_path_buf()).unwrap();
    hub.save(&sample()).unwrap();
    let Reply::Loaded(old) = hub.page_choice_call(profile, Call::Read) else {
        panic!()
    };
    assert_eq!(old.revision, 0);
    assert_eq!(
        hub.authorize_profile_deletion(profile, &SessionState::default())
            .unwrap(),
        ProfileDeletionAuthorizeOutcome::Authorized
    );
    assert!(hub
        .finalize_profile_deletion_requiring_restart_confirmation(profile)
        .unwrap());
    assert!(hub.save(&sample()).is_err());
    assert_eq!(
        hub.page_choice_call(
            profile,
            Call::CompareAndSwap {
                expected: Box::new(old.clone()),
                selection: Selection::Model(None),
            }
        ),
        Reply::Failed(Fault::NotRegistered)
    );
    drop(hub);
    let mut reopened = Hub::open(dir.path().to_path_buf()).unwrap();
    assert!(reopened.save(&sample()).is_err());
    assert_eq!(
        reopened.page_choice_call(
            profile,
            Call::CompareAndSwap {
                expected: Box::new(old),
                selection: Selection::Model(None),
            }
        ),
        Reply::Failed(Fault::NotRegistered)
    );
}
