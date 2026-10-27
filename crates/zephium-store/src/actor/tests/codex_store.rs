use super::*;
use zephium_core::codex_store::{Call, Dispatch, Fault, Reply};

#[test]
fn codex_store_actor_flushes_registered_profile_and_commits_exact_pending_record() {
    let mut cache = Connection::open_in_memory().unwrap();
    migrations::apply(&mut cache, migrations::PROFILE).unwrap();
    let store = SqliteStore::in_memory().unwrap();
    store.save_session(sample());
    let profile = ProfileId::from(1);
    let Reply::Scope(scope) = store.codex_call(
        profile,
        Call::Open {
            incarnation: [4; 16],
        },
    ) else {
        panic!("native profile scope")
    };
    let Reply::Association(Some(ready)) = store.codex_call(
        profile,
        Call::Bind {
            expected_revision: None,
            scope,
            conversation: "page-1".into(),
            thread_id: "original-thread".into(),
        },
    ) else {
        panic!("original association")
    };
    let Reply::Association(Some(pending)) = store.codex_call(
        profile,
        Call::Begin {
            scope,
            conversation: "page-1".into(),
            expected_revision: ready.revision,
            reply: 2,
        },
    ) else {
        panic!("durable pending")
    };
    assert!(matches!(
        pending.dispatch,
        Dispatch::Pending {
            reply: 2,
            turn_id: None
        }
    ));
    assert_eq!(
        store.codex_call(
            profile,
            Call::History {
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(pending))
    );
    assert_eq!(
        store.codex_call(
            ProfileId::from(999),
            Call::Open {
                incarnation: [4; 16]
            }
        ),
        Reply::Failed(Fault::NotRegistered)
    );
    assert!(store.flush());
}

#[test]
fn codex_store_lost_mutation_ack_is_unknown_and_closed_admission_is_definite_refusal() {
    let (tx, rx) = mpsc::sync_channel(1);
    let store = test_store_with_sender(tx);
    let receiver = std::thread::spawn(move || {
        let Cmd::Codex(_, _, reply) = rx.recv().unwrap() else {
            panic!("exact command")
        };
        // This models a lost acknowledgement, not proof no commit happened.
        drop(reply);
    });
    assert_eq!(
        store.codex_call(
            ProfileId::from(1),
            Call::Open {
                incarnation: [4; 16]
            }
        ),
        Reply::Failed(Fault::OutcomeUnknown)
    );
    receiver.join().unwrap();
    assert_eq!(
        store.codex_call(
            ProfileId::from(1),
            Call::Open {
                incarnation: [4; 16]
            }
        ),
        Reply::Failed(Fault::Unavailable)
    );
    store.lifecycle.write().unwrap().terminal_admitted = true;
    assert_eq!(
        store.codex_call(
            ProfileId::from(1),
            Call::History {
                conversation: "page-1".into()
            }
        ),
        Reply::Failed(Fault::Unavailable)
    );
}

#[test]
fn concurrent_begin_and_skip_cannot_both_consume_the_same_revision() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    store.save_session(sample());
    let profile = ProfileId::from(1);
    let Reply::Scope(scope) = store.codex_call(
        profile,
        Call::Open {
            incarnation: [4; 16],
        },
    ) else {
        panic!()
    };
    let Reply::Association(Some(ready)) = store.codex_call(
        profile,
        Call::Bind {
            scope,
            conversation: "page-1".into(),
            thread_id: "thread".into(),
            expected_revision: None,
        },
    ) else {
        panic!()
    };
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let call = |skip: bool| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let revision = ready.revision;
        std::thread::spawn(move || {
            barrier.wait();
            store.codex_call(
                profile,
                if skip {
                    Call::Skip {
                        scope,
                        conversation: "page-1".into(),
                        expected_revision: Some(revision),
                        reply: 2,
                    }
                } else {
                    Call::Begin {
                        scope,
                        conversation: "page-1".into(),
                        expected_revision: revision,
                        reply: 2,
                    }
                },
            )
        })
    };
    let begin = call(false);
    let skip = call(true);
    barrier.wait();
    let results = [begin.join().unwrap(), skip.join().unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Reply::Association(Some(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Reply::Failed(Fault::Conflict))
            .count(),
        1
    );
    let Reply::Association(Some(saved)) = store.codex_call(
        profile,
        Call::History {
            conversation: "page-1".into(),
        },
    ) else {
        panic!()
    };
    assert_eq!(saved.revision, ready.revision + 1);
    assert!(matches!(
        saved.dispatch,
        Dispatch::Pending {
            reply: 2,
            turn_id: None
        } | Dispatch::Skipped { reply: 2 }
    ));
}
#[test]
fn lost_skip_ack_is_outcome_unknown_not_a_durable_not_sent_receipt() {
    let (tx, rx) = mpsc::sync_channel(1);
    let store = test_store_with_sender(tx);
    let receiver = std::thread::spawn(move || {
        let Cmd::Codex(_, Call::Skip { .. }, reply) = rx.recv().unwrap() else {
            panic!("exact Skip command")
        };
        drop(reply);
    });
    assert_eq!(
        store.codex_call(
            ProfileId::from(1),
            Call::Skip {
                scope: zephium_core::codex_store::Scope {
                    incarnation: [4; 16],
                    grant_epoch: 1
                },
                conversation: "page-1".into(),
                expected_revision: None,
                reply: 2
            }
        ),
        Reply::Failed(Fault::OutcomeUnknown)
    );
    receiver.join().unwrap();
}
