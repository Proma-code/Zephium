use super::*;

fn setup() -> (Hub, ProfileId, Scope) {
    let mut hub = Hub::in_memory().unwrap();
    let profile = ProfileId::from(1);
    hub.registry.insert(profile);
    let Reply::Scope(scope) = hub.codex_call(
        profile,
        Call::Open {
            incarnation: [7; 16],
        },
    ) else {
        panic!("fresh scope")
    };
    (hub, profile, scope)
}
fn association(reply: Reply) -> Association {
    match reply {
        Reply::Association(Some(value)) => value,
        _ => panic!("association expected: {reply:?}"),
    }
}
fn bind(hub: &mut Hub, profile: ProfileId, scope: Scope) -> Association {
    association(hub.codex_call(
        profile,
        Call::Bind {
            expected_revision: None,
            scope,
            conversation: "page-1".into(),
            thread_id: "original-thread-1".into(),
        },
    ))
}
fn begin(hub: &mut Hub, profile: ProfileId, value: &Association, reply: u64) -> Association {
    association(hub.codex_call(
        profile,
        Call::Begin {
            scope: value.scope,
            conversation: value.conversation.clone(),
            expected_revision: value.revision,
            reply,
        },
    ))
}
#[test]
fn original_thread_continues_after_completed_turn_without_changing_home_epoch() {
    let (mut hub, profile, scope) = setup();
    let ready = bind(&mut hub, profile, scope);
    let pending = begin(&mut hub, profile, &ready, 2);
    let started = association(hub.codex_call(
        profile,
        Call::RecordTurn {
            scope,
            conversation: "page-1".into(),
            expected_revision: pending.revision,
            reply: 2,
            turn_id: "turn-1".into(),
        },
    ));
    let settled = association(hub.codex_call(
        profile,
        Call::Settle {
            scope,
            conversation: "page-1".into(),
            expected_revision: started.revision,
            reply: 2,
            turn_id: "turn-1".into(),
            outcome: Terminal::Completed,
        },
    ));
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Open {
                incarnation: [7; 16]
            }
        ),
        Reply::Scope(scope)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Load {
                scope,
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(settled.clone()))
    );
    let second = begin(&mut hub, profile, &settled, 4);
    assert_eq!(second.thread_id.as_deref(), Some("original-thread-1"));
    assert_eq!(second.scope, scope);
    assert!(matches!(
        second.dispatch,
        Dispatch::Pending {
            reply: 4,
            turn_id: None
        }
    ));
    let synchronous: i64 = hub
        .profile_conn(profile)
        .unwrap()
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    assert_eq!(synchronous, 1);
}
#[test]
fn uncertain_pending_cannot_be_replayed_reset_or_rebound() {
    let (mut hub, profile, scope) = setup();
    let ready = bind(&mut hub, profile, scope);
    let pending = begin(&mut hub, profile, &ready, 2);
    for call in [
        Call::Begin {
            scope,
            conversation: "page-1".into(),
            expected_revision: pending.revision,
            reply: 3,
        },
        Call::Bind {
            expected_revision: None,
            scope,
            conversation: "page-1".into(),
            thread_id: "replacement".into(),
        },
        Call::Bind {
            expected_revision: None,
            scope,
            conversation: "page-2".into(),
            thread_id: "original-thread-1".into(),
        },
        Call::RecordTurn {
            scope,
            conversation: "page-1".into(),
            expected_revision: ready.revision,
            reply: 2,
            turn_id: "turn-1".into(),
        },
    ] {
        assert_eq!(
            hub.codex_call(profile, call),
            Reply::Failed(Fault::Conflict)
        );
    }
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Load {
                scope,
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(pending))
    );
}
#[test]
fn exact_terminal_can_reconcile_pending_but_not_a_different_original_turn() {
    let (mut hub, profile, scope) = setup();
    let ready = bind(&mut hub, profile, scope);
    let pending = begin(&mut hub, profile, &ready, 2);
    let recorded = association(hub.codex_call(
        profile,
        Call::RecordTurn {
            scope,
            conversation: "page-1".into(),
            expected_revision: pending.revision,
            reply: 2,
            turn_id: "original".into(),
        },
    ));
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Settle {
                scope,
                conversation: "page-1".into(),
                expected_revision: recorded.revision,
                reply: 2,
                turn_id: "other".into(),
                outcome: Terminal::Completed
            }
        ),
        Reply::Failed(Fault::Conflict)
    );
    let terminal = association(hub.codex_call(
        profile,
        Call::Settle {
            scope,
            conversation: "page-1".into(),
            expected_revision: recorded.revision,
            reply: 2,
            turn_id: "original".into(),
            outcome: Terminal::Interrupted,
        },
    ));
    assert!(matches!(
        terminal.dispatch,
        Dispatch::Settled {
            outcome: Terminal::Interrupted,
            ..
        }
    ));
}
#[test]
fn auth_epoch_cas_retires_old_authority_without_deleting_conversation_or_visible_history() {
    let (mut hub, profile, scope) = setup();
    let ready = bind(&mut hub, profile, scope);
    assert!(hub.save_chat(
        profile,
        "page-1",
        "space",
        "title",
        1,
        "synthetic visible history"
    ));
    let Reply::Scope(next) = hub.codex_call(profile, Call::AdvanceGrant { expected: scope }) else {
        panic!("advance")
    };
    assert_eq!(next.grant_epoch, 2);
    assert_eq!(
        hub.codex_call(profile, Call::AdvanceGrant { expected: scope }),
        Reply::Failed(Fault::StaleGrant)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Load {
                scope: next,
                conversation: "page-1".into()
            }
        ),
        Reply::Failed(Fault::StaleGrant)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::History {
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(ready.clone()))
    );
    assert_eq!(
        read(hub.profile_conn(profile).unwrap(), "page-1").unwrap(),
        Some(ready)
    );
    assert_eq!(
        hub.chat(profile, "page-1").as_deref(),
        Some("synthetic visible history")
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Open {
                incarnation: [8; 16]
            }
        ),
        Reply::Failed(Fault::IncarnationMismatch)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Open {
                incarnation: [7; 16]
            }
        ),
        Reply::Scope(next)
    );
}
#[test]
fn wrong_profile_invalid_ids_degraded_and_failed_write_never_produce_success() {
    let (mut hub, profile, scope) = setup();
    assert_eq!(
        hub.codex_call(
            ProfileId::from(2),
            Call::Open {
                incarnation: [1; 16]
            }
        ),
        Reply::Failed(Fault::NotRegistered)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Load {
                scope,
                conversation: "../page".into()
            }
        ),
        Reply::Failed(Fault::Invalid)
    );
    hub.degraded_profiles.insert(profile);
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Open {
                incarnation: [7; 16]
            }
        ),
        Reply::Failed(Fault::Unavailable)
    );
    hub.degraded_profiles.remove(&profile);
    hub.profile_conn(profile)
        .unwrap()
        .execute_batch("PRAGMA query_only=ON")
        .unwrap();
    assert_eq!(
        hub.codex_call(profile, Call::AdvanceGrant { expected: scope }),
        Reply::Failed(Fault::OutcomeUnknown)
    );
    hub.profile_conn(profile)
        .unwrap()
        .execute_batch("PRAGMA query_only=OFF")
        .unwrap();
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Open {
                incarnation: [7; 16]
            }
        ),
        Reply::Scope(scope)
    );
}
#[test]
fn disk_restart_preserves_pending_and_exact_scope_without_runtime_version_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, migrations::PROFILE).unwrap();
    let Reply::Scope(scope) = execute(
        &conn,
        Call::Open {
            incarnation: [7; 16],
        },
    )
    .unwrap() else {
        panic!()
    };
    let ready = association(
        execute(
            &conn,
            Call::Bind {
                expected_revision: None,
                scope,
                conversation: "page-1".into(),
                thread_id: "thread-1".into(),
            },
        )
        .unwrap(),
    );
    let pending = association(
        execute(
            &conn,
            Call::Begin {
                scope,
                conversation: "page-1".into(),
                expected_revision: ready.revision,
                reply: 2,
            },
        )
        .unwrap(),
    );
    drop(conn);
    let mut conn = Connection::open(&path).unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, migrations::PROFILE).unwrap();
    assert_eq!(
        execute(
            &conn,
            Call::Open {
                incarnation: [7; 16]
            }
        )
        .unwrap(),
        Reply::Scope(scope)
    );
    assert_eq!(
        execute(
            &conn,
            Call::Load {
                scope,
                conversation: "page-1".into()
            }
        )
        .unwrap(),
        Reply::Association(Some(pending))
    );
    conn.pragma_update(None, "user_version", 39).unwrap();
    assert!(migrations::validate_current(&conn, migrations::PROFILE).is_err());
}
#[test]
fn explicit_history_deletion_removes_association_but_does_not_reset_account_epoch() {
    let (mut hub, profile, scope) = setup();
    bind(&mut hub, profile, scope);
    assert!(hub.save_chat(profile, "page-1", "space", "title", 1, "history"));
    assert!(hub.delete_chat(profile, "page-1"));
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Load {
                scope,
                conversation: "page-1".into()
            }
        ),
        Reply::Association(None)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Open {
                incarnation: [7; 16]
            }
        ),
        Reply::Scope(scope)
    );
}
#[test]
fn migration_from_delivered_chat_schema_preserves_bodies_and_adds_no_account_identity() {
    let mut conn = Connection::open_in_memory().unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, &migrations::PROFILE[..36]).unwrap();
    conn.execute("INSERT INTO chats(id,space,title,updated_ms,body) VALUES('page-1','s','t',1,'existing history')",[]).unwrap();
    migrations::apply(&mut conn, migrations::PROFILE).unwrap();
    assert_eq!(
        conn.query_row("SELECT body FROM chats WHERE id='page-1'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "existing history"
    );
    assert_eq!(scope(&conn).unwrap(), None);
    assert_eq!(read(&conn, "page-1").unwrap(), None);
    assert_eq!(
        execute(
            &conn,
            Call::History {
                conversation: "page-1".into()
            }
        )
        .unwrap(),
        Reply::Association(None)
    );
}
#[test]
fn association_count_is_bounded_and_stale_grants_cannot_evict_saved_history() {
    let (mut hub, profile, scope) = setup();
    for index in 0..MAX_ASSOCIATIONS {
        assert!(matches!(
            hub.codex_call(
                profile,
                Call::Bind {
                    expected_revision: None,
                    scope,
                    conversation: format!("page-{index}"),
                    thread_id: format!("thread-{index}")
                }
            ),
            Reply::Association(Some(_))
        ));
    }
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Bind {
                expected_revision: None,
                scope,
                conversation: "overflow".into(),
                thread_id: "overflow".into()
            }
        ),
        Reply::Failed(Fault::Limit)
    );
    assert!(matches!(
        hub.codex_call(
            profile,
            Call::Load {
                scope,
                conversation: "page-0".into()
            }
        ),
        Reply::Association(Some(_))
    ));
}
#[test]
fn pending_before_transcript_save_survives_unrelated_chat_pruning() {
    let (mut hub, profile, scope) = setup();
    let ready = bind(&mut hub, profile, scope);
    let pending = begin(&mut hub, profile, &ready, 2);
    // Thread creation/dispatch durability may precede asynchronous Page body
    // persistence. Absence from chats is not permission to delete this record.
    for index in 0..=zephium_core::ports::store::MAX_CHATS {
        assert!(hub.save_chat(
            profile,
            &format!("other-{index}"),
            "s",
            "t",
            index as i64,
            "{}"
        ));
    }
    assert_eq!(hub.chat(profile, "other-0"), None);
    assert_eq!(
        hub.codex_call(
            profile,
            Call::History {
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(pending))
    );
}

fn skip(
    hub: &mut Hub,
    profile: ProfileId,
    scope: Scope,
    revision: Option<u64>,
    reply: u64,
) -> Reply {
    hub.codex_call(
        profile,
        Call::Skip {
            scope,
            conversation: "page-1".into(),
            expected_revision: revision,
            reply,
        },
    )
}
#[test]
fn proven_unsent_before_thread_retains_reply_and_original_receipt_fills_only_exact_cas() {
    let (mut hub, profile, scope) = setup();
    let skipped = association(skip(&mut hub, profile, scope, None, 2));
    assert_eq!(skipped.thread_id, None);
    assert_eq!(skipped.dispatch, Dispatch::Skipped { reply: 2 });
    assert_eq!(
        skip(&mut hub, profile, scope, None, 2),
        Reply::Failed(Fault::Conflict)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Begin {
                scope,
                conversation: "page-1".into(),
                expected_revision: skipped.revision,
                reply: 4
            }
        ),
        Reply::Failed(Fault::Conflict)
    );
    for revision in [None, Some(skipped.revision + 1)] {
        assert_eq!(
            hub.codex_call(
                profile,
                Call::Bind {
                    scope,
                    conversation: "page-1".into(),
                    thread_id: "original-thread".into(),
                    expected_revision: revision
                }
            ),
            Reply::Failed(Fault::Conflict)
        );
    }
    let bound = association(hub.codex_call(
        profile,
        Call::Bind {
            scope,
            conversation: "page-1".into(),
            thread_id: "original-thread".into(),
            expected_revision: Some(skipped.revision),
        },
    ));
    assert_eq!(bound.thread_id.as_deref(), Some("original-thread"));
    assert_eq!(bound.dispatch, skipped.dispatch);
    assert_eq!(bound.revision, skipped.revision + 1);
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Bind {
                scope,
                conversation: "page-1".into(),
                thread_id: "replacement".into(),
                expected_revision: Some(bound.revision)
            }
        ),
        Reply::Failed(Fault::Conflict)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Begin {
                scope,
                conversation: "page-1".into(),
                expected_revision: bound.revision,
                reply: 2
            }
        ),
        Reply::Failed(Fault::Conflict)
    );
    let next = begin(&mut hub, profile, &bound, 4);
    assert_eq!(next.thread_id, bound.thread_id);
    assert_eq!(
        next.dispatch,
        Dispatch::Pending {
            reply: 4,
            turn_id: None
        }
    );
}
#[test]
fn skip_requires_exact_reply_revision_and_no_observed_original_turn() {
    let (mut hub, profile, scope) = setup();
    let ready = bind(&mut hub, profile, scope);
    let pending = begin(&mut hub, profile, &ready, 2);
    for (revision, reply) in [
        (None, 2),
        (Some(ready.revision), 2),
        (Some(pending.revision), 3),
    ] {
        assert_eq!(
            skip(&mut hub, profile, scope, revision, reply),
            Reply::Failed(Fault::Conflict)
        );
    }
    let skipped = association(skip(&mut hub, profile, scope, Some(pending.revision), 2));
    assert_eq!(skipped.thread_id, pending.thread_id);
    assert_eq!(skipped.dispatch, Dispatch::Skipped { reply: 2 });
    assert_eq!(
        skip(&mut hub, profile, scope, Some(pending.revision), 2),
        Reply::Failed(Fault::Conflict)
    );
    assert_eq!(
        skip(&mut hub, profile, scope, Some(skipped.revision), 2),
        Reply::Failed(Fault::Conflict)
    );
    let pending = begin(&mut hub, profile, &skipped, 4);
    let started = association(hub.codex_call(
        profile,
        Call::RecordTurn {
            scope,
            conversation: "page-1".into(),
            expected_revision: pending.revision,
            reply: 4,
            turn_id: "actual-turn".into(),
        },
    ));
    assert_eq!(
        skip(&mut hub, profile, scope, Some(started.revision), 4),
        Reply::Failed(Fault::Conflict)
    );
    assert_eq!(
        read(hub.profile_conn(profile).unwrap(), "page-1").unwrap(),
        Some(started)
    );
}
#[test]
fn skipped_history_survives_retired_grant_without_becoming_current_resume_authority() {
    let (mut hub, profile, scope) = setup();
    let skipped = association(skip(&mut hub, profile, scope, None, 2));
    assert!(hub.save_chat(
        profile,
        "page-1",
        "s",
        "t",
        1,
        "visible attempted words, not sent"
    ));
    let Reply::Scope(next) = hub.codex_call(profile, Call::AdvanceGrant { expected: scope }) else {
        panic!()
    };
    assert_eq!(
        skip(&mut hub, profile, scope, Some(skipped.revision), 4),
        Reply::Failed(Fault::StaleGrant)
    );
    assert_eq!(
        skip(&mut hub, profile, next, Some(skipped.revision), 4),
        Reply::Failed(Fault::StaleGrant)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Load {
                scope: next,
                conversation: "page-1".into()
            }
        ),
        Reply::Failed(Fault::StaleGrant)
    );
    assert_eq!(
        hub.codex_call(
            profile,
            Call::History {
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(skipped))
    );
    assert_eq!(
        hub.chat(profile, "page-1").as_deref(),
        Some("visible attempted words, not sent")
    );
}
#[test]
fn failed_skip_write_never_acknowledges_not_sent_or_changes_prior_record() {
    let (mut hub, profile, scope) = setup();
    let ready = bind(&mut hub, profile, scope);
    hub.profile_conn(profile)
        .unwrap()
        .execute_batch("PRAGMA query_only=ON")
        .unwrap();
    assert_eq!(
        skip(&mut hub, profile, scope, Some(ready.revision), 2),
        Reply::Failed(Fault::OutcomeUnknown)
    );
    hub.profile_conn(profile)
        .unwrap()
        .execute_batch("PRAGMA query_only=OFF")
        .unwrap();
    assert_eq!(
        read(hub.profile_conn(profile).unwrap(), "page-1").unwrap(),
        Some(ready)
    );
}
#[test]
fn v37_to_v38_preserves_every_existing_identity_revision_unknown_and_visible_history() {
    let mut conn = Connection::open_in_memory().unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, &migrations::PROFILE[..37]).unwrap();
    conn.execute(
        "INSERT INTO codex_owner VALUES(1,1,?1,2)",
        [[7_u8; 16].as_slice()],
    )
    .unwrap();
    for (name, grant, state, reply, turn, outcome) in [
        ("ready", 2, 0, 0, None, None),
        ("pending", 2, 1, 2, None, None),
        ("started", 2, 1, 4, Some("original-turn"), None),
        ("retired", 1, 2, 6, Some("old-turn"), Some(1)),
    ] {
        conn.execute(
            "INSERT INTO codex_conversations VALUES(?1,?2,?3,?4,9,?5,?6,?7,?8)",
            params![
                name,
                [7_u8; 16].as_slice(),
                grant,
                format!("thread-{name}"),
                state,
                reply,
                turn,
                outcome
            ],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO chats VALUES('retired','s','t',1,'kept visible history')",
        [],
    )
    .unwrap();
    let names = ["ready", "pending", "started", "retired"];
    let before: Vec<_> = names
        .iter()
        .map(|name| read(&conn, name).unwrap())
        .collect();
    let owner = scope(&conn).unwrap();
    migrations::apply(&mut conn, migrations::PROFILE).unwrap();
    assert_eq!(scope(&conn).unwrap(), owner);
    assert_eq!(
        names
            .iter()
            .map(|name| read(&conn, name).unwrap())
            .collect::<Vec<_>>(),
        before
    );
    assert_eq!(
        conn.query_row("SELECT body FROM chats WHERE id='retired'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "kept visible history"
    );
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        38
    );
    let current = owner.unwrap();
    assert_eq!(
        execute(
            &conn,
            Call::Begin {
                scope: current,
                conversation: "pending".into(),
                expected_revision: 9,
                reply: 10
            }
        )
        .unwrap(),
        Reply::Failed(Fault::Conflict)
    );
    assert_eq!(
        execute(
            &conn,
            Call::Load {
                scope: current,
                conversation: "retired".into()
            }
        )
        .unwrap(),
        Reply::Failed(Fault::StaleGrant)
    );
}
#[test]
fn v38_rebuild_failure_rolls_back_without_changing_v37_schema_or_rows() {
    let mut conn = Connection::open_in_memory().unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, &migrations::PROFILE[..37]).unwrap();
    conn.execute(
        "INSERT INTO codex_owner VALUES(1,1,?1,1)",
        [[7_u8; 16].as_slice()],
    )
    .unwrap();
    // Deliberately corrupt a synthetic old row to force the replacement-table
    // copy to fail. No generic migration success or data repair is inferred.
    conn.execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    conn.execute(
        "INSERT INTO codex_conversations VALUES('page-1',?1,1,'',1,1,2,NULL,NULL)",
        [[7_u8; 16].as_slice()],
    )
    .unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF")
        .unwrap();
    assert!(migrations::apply(&mut conn, migrations::PROFILE).is_err());
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        37
    );
    migrations::validate_current(&conn, &migrations::PROFILE[..37]).unwrap();
    assert_eq!(
        conn.query_row("SELECT thread_id FROM codex_conversations", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        ""
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='codex_conversations_v37'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn schema_allows_absent_thread_only_for_explicit_skipped_and_preserves_unique_original_ids() {
    let (mut hub, profile, _) = setup();
    let conn = hub.profile_conn(profile).unwrap();
    for (state, reply, turn, outcome) in [
        (0, 0, None, None),
        (1, 2, None, None),
        (2, 2, Some("turn"), Some(1)),
    ] {
        assert!(
            conn.execute(
                "INSERT INTO codex_conversations VALUES('bad',?1,1,NULL,1,?2,?3,?4,?5)",
                params![[7_u8; 16].as_slice(), state, reply, turn, outcome]
            )
            .is_err()
        );
    }
    for page in ["skip-a", "skip-b"] {
        conn.execute(
            "INSERT INTO codex_conversations VALUES(?1,?2,1,NULL,1,3,2,NULL,NULL)",
            params![page, [7_u8; 16].as_slice()],
        )
        .unwrap();
    }
    assert!(
        conn.execute(
            "UPDATE codex_conversations SET turn_id='not-unsent' WHERE conversation='skip-a'",
            []
        )
        .is_err()
    );
    conn.execute(
        "UPDATE codex_conversations SET thread_id='owned' WHERE conversation='skip-a'",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE codex_conversations SET thread_id='owned' WHERE conversation='skip-b'",
            []
        )
        .is_err()
    );
}
#[test]
fn skipped_none_reopens_as_history_not_a_thread_or_permission_to_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("continuity.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, migrations::PROFILE).unwrap();
    let Reply::Scope(scope) = execute(
        &conn,
        Call::Open {
            incarnation: [7; 16],
        },
    )
    .unwrap() else {
        panic!()
    };
    let skipped = association(
        execute(
            &conn,
            Call::Skip {
                scope,
                conversation: "page-1".into(),
                expected_revision: None,
                reply: 2,
            },
        )
        .unwrap(),
    );
    drop(conn);
    let mut conn = Connection::open(&path).unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, migrations::PROFILE).unwrap();
    assert_eq!(
        execute(
            &conn,
            Call::Open {
                incarnation: [7; 16]
            }
        )
        .unwrap(),
        Reply::Scope(scope)
    );
    assert_eq!(
        execute(
            &conn,
            Call::History {
                conversation: "page-1".into()
            }
        )
        .unwrap(),
        Reply::Association(Some(skipped.clone()))
    );
    assert_eq!(
        execute(
            &conn,
            Call::Begin {
                scope,
                conversation: "page-1".into(),
                expected_revision: skipped.revision,
                reply: 4
            }
        )
        .unwrap(),
        Reply::Failed(Fault::Conflict)
    );
    assert_eq!(
        execute(
            &conn,
            Call::Skip {
                scope,
                conversation: "page-1".into(),
                expected_revision: Some(skipped.revision),
                reply: 2
            }
        )
        .unwrap(),
        Reply::Failed(Fault::Conflict)
    );
    assert_eq!(
        execute(
            &conn,
            Call::Skip {
                scope: Scope {
                    incarnation: [8; 16],
                    ..scope
                },
                conversation: "page-1".into(),
                expected_revision: Some(skipped.revision),
                reply: 4
            }
        )
        .unwrap(),
        Reply::Failed(Fault::IncarnationMismatch)
    );
}
