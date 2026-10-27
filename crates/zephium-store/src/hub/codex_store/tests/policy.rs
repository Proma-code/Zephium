use super::*;
fn browser(tools: u8, instructions: u8) -> ConversationPolicy {
    ConversationPolicy::BrowserV1 {
        tool_manifest_sha256: [tools; 32],
        instruction_policy_sha256: [instructions; 32],
    }
}
#[test]
fn every_thread_operation_requires_exact_policy_without_retiring_the_account() {
    let (mut hub, profile, scope) = setup();
    let policy = browser(1, 2);
    let ready = association(hub.codex_call(
        profile,
        Call::Bind {
            scope,
            policy,
            conversation: "page-1".into(),
            thread_id: "original-browser-thread".into(),
            expected_revision: None,
        },
    ));
    for wrong in [
        ConversationPolicy::TextOnlyLegacy,
        browser(3, 2),
        browser(1, 4),
    ] {
        for call in [
            Call::Load {
                scope,
                policy: wrong,
                conversation: "page-1".into(),
            },
            Call::Bind {
                scope,
                policy: wrong,
                conversation: "page-1".into(),
                thread_id: "replacement-thread".into(),
                expected_revision: Some(1),
            },
            Call::Skip {
                scope,
                policy: wrong,
                conversation: "page-1".into(),
                expected_revision: Some(1),
                reply: 2,
            },
            Call::Begin {
                scope,
                policy: wrong,
                conversation: "page-1".into(),
                expected_revision: 1,
                reply: 2,
            },
            Call::RecordTurn {
                scope,
                policy: wrong,
                conversation: "page-1".into(),
                expected_revision: 1,
                reply: 2,
                turn_id: "turn-1".into(),
            },
            Call::Settle {
                scope,
                policy: wrong,
                conversation: "page-1".into(),
                expected_revision: 1,
                reply: 2,
                turn_id: "turn-1".into(),
                outcome: Terminal::Completed,
            },
        ] {
            assert_eq!(
                hub.codex_call(profile, call),
                Reply::Failed(Fault::PolicyMismatch)
            );
        }
        assert_eq!(
            read(hub.profile_conn(profile).unwrap(), "page-1").unwrap(),
            Some(ready.clone())
        );
        assert_eq!(
            hub.codex_call(profile, Call::InspectOwner),
            Reply::Owner(Some(scope))
        );
    }
    let pending = association(hub.codex_call(
        profile,
        Call::Begin {
            scope,
            policy,
            conversation: "page-1".into(),
            expected_revision: ready.revision,
            reply: 2,
        },
    ));
    let settled = association(hub.codex_call(
        profile,
        Call::Settle {
            scope,
            policy,
            conversation: "page-1".into(),
            expected_revision: pending.revision,
            reply: 2,
            turn_id: "turn-1".into(),
            outcome: Terminal::Completed,
        },
    ));
    assert_eq!(settled.policy, policy);
    assert_eq!(
        hub.codex_call(
            profile,
            Call::History {
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(settled))
    );
}
#[test]
fn skipped_before_thread_seals_policy_and_binding_cannot_upgrade_or_erase_receipt() {
    let (mut hub, profile, scope) = setup();
    let policy = browser(1, 2);
    let skipped = association(hub.codex_call(
        profile,
        Call::Skip {
            scope,
            policy,
            conversation: "page-1".into(),
            expected_revision: None,
            reply: 2,
        },
    ));
    assert!(skipped.thread_id.is_none());
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Bind {
                scope,
                policy: browser(3, 2),
                conversation: "page-1".into(),
                thread_id: "original".into(),
                expected_revision: Some(skipped.revision)
            }
        ),
        Reply::Failed(Fault::PolicyMismatch)
    );
    let bound = association(hub.codex_call(
        profile,
        Call::Bind {
            scope,
            policy,
            conversation: "page-1".into(),
            thread_id: "original".into(),
            expected_revision: Some(skipped.revision),
        },
    ));
    assert_eq!(bound.policy, policy);
    assert_eq!(bound.dispatch, Dispatch::Skipped { reply: 2 });
    let (mut old, p, scope) = setup();
    let prior = bind(&mut old, p, scope);
    assert_eq!(
        old.codex_call(
            p,
            Call::Load {
                scope,
                policy,
                conversation: "page-1".into()
            }
        ),
        Reply::Failed(Fault::PolicyMismatch)
    );
    assert_eq!(
        old.codex_call(
            p,
            Call::History {
                conversation: "page-1".into()
            }
        ),
        Reply::Association(Some(prior))
    );
}
#[test]
fn v38_to_v39_preserves_all_states_and_explicitly_tags_every_existing_row_text_only() {
    let mut conn = Connection::open_in_memory().unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, &migrations::PROFILE[..38]).unwrap();
    conn.execute(
        "INSERT INTO codex_owner VALUES(1,1,?1,2)",
        [[7u8; 16].as_slice()],
    )
    .unwrap();
    for (name, grant, thread, state, reply, turn, outcome) in [
        ("ready", 2, Some("t-ready"), 0, 0, None, None),
        ("pending", 2, Some("t-pending"), 1, 2, None, None),
        (
            "started",
            2,
            Some("t-started"),
            1,
            4,
            Some("original-turn"),
            None,
        ),
        (
            "retired",
            1,
            Some("t-retired"),
            2,
            6,
            Some("old-turn"),
            Some(1),
        ),
        ("skip-none", 2, None, 3, 8, None, None),
        ("skip-thread", 2, Some("t-skipped"), 3, 10, None, None),
    ] {
        conn.execute(
            "INSERT INTO codex_conversations VALUES(?1,?2,?3,?4,9,?5,?6,?7,?8)",
            params![
                name,
                [7u8; 16].as_slice(),
                grant,
                thread,
                state,
                reply,
                turn,
                outcome
            ],
        )
        .unwrap();
    }
    let names = [
        "ready",
        "pending",
        "started",
        "retired",
        "skip-none",
        "skip-thread",
    ];
    let before: Vec<_> = names
        .iter()
        .map(|name| read_legacy_v38(&conn, name).unwrap())
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
    for name in names {
        assert_eq!(
            read(&conn, name).unwrap().unwrap().policy,
            ConversationPolicy::TextOnlyLegacy
        );
    }
    conn.execute_batch("PRAGMA user_version=40").unwrap();
    assert!(migrations::apply(&mut conn, migrations::PROFILE).is_err());
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        40
    );
}
#[test]
fn v39_rebuild_failure_rolls_back_exact_v38_and_policy_corruption_is_never_normalized() {
    let mut conn = Connection::open_in_memory().unwrap();
    filesystem::configure_profile(&conn).unwrap();
    migrations::apply(&mut conn, &migrations::PROFILE[..38]).unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    conn.execute(
        "INSERT INTO codex_conversations VALUES('bad',?1,1,NULL,1,9,2,NULL,NULL)",
        [[7u8; 16].as_slice()],
    )
    .unwrap();
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF")
        .unwrap();
    assert!(migrations::apply(&mut conn, migrations::PROFILE).is_err());
    migrations::validate_current(&conn, &migrations::PROFILE[..38]).unwrap();
    assert_eq!(
        conn.query_row("SELECT dispatch FROM codex_conversations", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='codex_conversations_v38'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    for sql in [
        "policy_kind=7",
        "policy_kind=1,tool_manifest_sha256=NULL,instruction_policy_sha256=NULL",
        "policy_kind=1,tool_manifest_sha256=zeroblob(32),instruction_policy_sha256=zeroblob(32)",
        "tool_manifest_sha256=zeroblob(32)",
    ] {
        let (mut hub, profile, _) = setup();
        let owner = scope(hub.profile_conn(profile).unwrap()).unwrap().unwrap();
        bind(&mut hub, profile, owner);
        let c = hub.profile_conn(profile).unwrap();
        c.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        c.execute(&format!("UPDATE codex_conversations SET {sql}"), [])
            .unwrap();
        c.execute_batch("PRAGMA ignore_check_constraints=OFF")
            .unwrap();
        assert!(read(c, "page-1").is_err());
        assert_eq!(
            hub.codex_call(profile, Call::InspectOwner),
            Reply::Failed(Fault::Corrupt)
        );
    }
}
#[test]
fn failed_policy_write_cannot_acknowledge_binding_and_sql_upsert_cannot_replace_it() {
    let (mut hub, profile, scope) = setup();
    let policy = browser(1, 2);
    let c = hub.profile_conn(profile).unwrap();
    c.execute_batch("PRAGMA query_only=ON").unwrap();
    assert_eq!(
        hub.codex_call(
            profile,
            Call::Skip {
                scope,
                policy,
                conversation: "page-1".into(),
                expected_revision: None,
                reply: 2
            }
        ),
        Reply::Failed(Fault::OutcomeUnknown)
    );
    hub.profile_conn(profile)
        .unwrap()
        .execute_batch("PRAGMA query_only=OFF")
        .unwrap();
    assert_eq!(
        read(hub.profile_conn(profile).unwrap(), "page-1").unwrap(),
        None
    );
    let original = association(hub.codex_call(
        profile,
        Call::Skip {
            scope,
            policy,
            conversation: "page-1".into(),
            expected_revision: None,
            reply: 2,
        },
    ));
    let mut replacement = original.clone();
    replacement.policy = browser(9, 2);
    replacement.revision += 1;
    assert!(put(hub.profile_conn(profile).unwrap(), &replacement).is_err());
    assert_eq!(
        read(hub.profile_conn(profile).unwrap(), "page-1").unwrap(),
        Some(original)
    );
}
