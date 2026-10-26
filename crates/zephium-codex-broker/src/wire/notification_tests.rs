use super::*;

fn fixtures() -> Vec<(&'static str, Value, AdvisoryKind, AdvisoryScope<'static>)> {
    use AdvisoryKind as K;
    use AdvisoryScope as S;
    vec![
        ("skills/changed", json!({}), K::SkillsChanged, S::Global),
        (
            "thread/readState/changed",
            json!({"threadId":"t","readState":{"revision":"r","firstUnread":{"type":"turn","turnId":"prior"}}}),
            K::ThreadReadState,
            S::Thread("t"),
        ),
        (
            "thread/queue/changed",
            json!({"threadId":"t"}),
            K::ThreadQueue,
            S::Thread("t"),
        ),
        (
            "account/rateLimits/updated",
            json!({"rateLimits":{"primary":{"usedPercent":12,"windowDurationMins":60,"resetsAt":123},"secondary":null,"credits":{"hasCredits":true,"unlimited":false,"balance":null},"individualLimit":null}}),
            K::RateLimits,
            S::Global,
        ),
        (
            "thread/started",
            json!({"thread":{"id":"t","futurePresentation":null}}),
            K::ThreadStarted,
            S::Thread("t"),
        ),
        (
            "thread/status/changed",
            json!({"threadId":"t","status":{"type":"active","activeFlags":[]}}),
            K::ThreadStatus,
            S::Thread("t"),
        ),
        (
            "thread/name/updated",
            json!({"threadId":"t","threadName":"Synthetic title"}),
            K::ThreadName,
            S::Thread("t"),
        ),
        (
            "warning",
            json!({"threadId":null,"message":"Synthetic warning\nNot projected"}),
            K::Warning,
            S::Global,
        ),
        (
            "guardianWarning",
            json!({"threadId":"t","message":"Synthetic warning"}),
            K::GuardianWarning,
            S::Thread("t"),
        ),
        (
            "deprecationNotice",
            json!({"summary":"Synthetic summary","details":null}),
            K::Deprecation,
            S::Global,
        ),
        (
            "model/verification",
            json!({"threadId":"t","turnId":"u","verifications":["trustedAccessForCyber"]}),
            K::ModelVerification,
            S::Turn {
                thread_id: "t",
                turn_id: "u",
            },
        ),
        (
            "turn/moderationMetadata",
            json!({"threadId":"t","turnId":"u","metadata":{"synthetic":true}}),
            K::ModerationMetadata,
            S::Turn {
                thread_id: "t",
                turn_id: "u",
            },
        ),
        (
            "model/safetyBuffering/updated",
            json!({"threadId":"t","turnId":"u","model":"model","useCases":["cyber"],"reasons":["user_risk"],"showBufferingUi":true,"fasterModel":null}),
            K::SafetyBuffering,
            S::Turn {
                thread_id: "t",
                turn_id: "u",
            },
        ),
        (
            "modelProvider/authRecoveryStarted",
            json!({"threadId":"t","turnId":"u","provider":"openai","message":"Synthetic recovery"}),
            K::AuthRecoveryStarted,
            S::Turn {
                thread_id: "t",
                turn_id: "u",
            },
        ),
        (
            "modelProvider/authRecoveryCompleted",
            json!({"threadId":"t","turnId":"u","provider":"openai","message":"Synthetic recovery"}),
            K::AuthRecoveryCompleted,
            S::Turn {
                thread_id: "t",
                turn_id: "u",
            },
        ),
    ]
}
#[test]
fn pinned_informational_shapes_are_typed_scoped_and_values_are_not_returned() {
    for (method, mut params, kind, scope) in fixtures() {
        assert_eq!(
            classify_notification(method, &params),
            Ok(NotificationClass::Informational(Advisory { kind, scope })),
            "{method}"
        );
        params["futurePresentation"] =
            json!({"nested":[null,0,"synthetic"],"authMode":"not authority"});
        assert_eq!(
            classify_notification(method, &params),
            Ok(NotificationClass::Informational(Advisory { kind, scope })),
            "additive {method}"
        );
        let bytes =
            serde_json::to_vec(&json!({"method":method,"params":params,"emittedAtMs":1})).unwrap();
        let Incoming::Notification { method, params } = decode(&bytes).unwrap() else {
            panic!("notification")
        };
        assert!(matches!(
            classify_notification(&method, &params),
            Ok(NotificationClass::Informational(_))
        ));
    }
}
#[test]
fn required_payload_types_and_scope_cannot_be_hidden_by_additive_fields() {
    for (method, mut params, _, _) in fixtures() {
        if method == "skills/changed" {
            assert!(classify_notification(method, &Value::Null).is_err());
            continue;
        }
        let field = match method {
            "thread/readState/changed" => "readState",
            "thread/queue/changed" => "threadId",
            "account/rateLimits/updated" => "rateLimits",
            "thread/started" => "thread",
            "thread/status/changed" => "status",
            "thread/name/updated" => "threadId",
            "warning" | "guardianWarning" => "message",
            "deprecationNotice" => "summary",
            "model/verification" => "verifications",
            "turn/moderationMetadata" => "threadId",
            "model/safetyBuffering/updated" => "showBufferingUi",
            _ => "provider",
        };
        params[field] = json!(19);
        assert!(
            classify_notification(method, &params).is_err(),
            "{method}/{field}"
        );
    }
    for params in [
        json!({"threadId":null,"turnId":"u","verifications":[]}),
        json!({"threadId":"t","turnId":null,"verifications":[]}),
        json!({"threadId":"t\n","turnId":"u","verifications":[]}),
    ] {
        assert_eq!(
            classify_notification("model/verification", &params),
            Err(Fault::Envelope)
        );
    }
    assert_eq!(
        classify_notification(
            "thread/started",
            &json!({"thread":{"id":"t"},"threadId":"foreign"})
        ),
        Err(Fault::Envelope)
    );
}
#[test]
fn original_optional_and_arbitrary_metadata_shapes_remain_optional_and_inert() {
    for value in [
        json!({"threadId":"t"}),
        json!({"threadId":"t","threadName":null}),
    ] {
        assert!(classify_notification("thread/name/updated", &value).is_ok());
    }
    assert!(classify_notification("warning", &json!({"message":""})).is_ok());
    assert!(classify_notification("deprecationNotice", &json!({"summary":""})).is_ok());
    for metadata in [
        Value::Null,
        json!(true),
        json!(12.5),
        json!("inert"),
        json!([]),
        json!({}),
    ] {
        assert!(
            classify_notification(
                "turn/moderationMetadata",
                &json!({"threadId":"t","turnId":"u","metadata":metadata})
            )
            .is_ok()
        );
    }
    assert!(
        classify_notification(
            "turn/moderationMetadata",
            &json!({"threadId":"t","turnId":"u"})
        )
        .is_err()
    );
    // An additive verification label is NOT a local privilege/capability grant.
    assert!(matches!(
        classify_notification(
            "model/verification",
            &json!({"threadId":"t","turnId":"u","verifications":["futureUninterpretedLabel"]})
        ),
        Ok(NotificationClass::Informational(_))
    ));
}
#[test]
fn account_state_requests_policy_and_terminal_are_never_informational() {
    for (method, class) in [
        ("account/updated", NotificationClass::AccountInvalidation),
        (
            "account/login/completed",
            NotificationClass::AccountLoginCompleted,
        ),
        (
            "remoteControl/status/changed",
            NotificationClass::RemoteAuthority,
        ),
    ] {
        assert_eq!(classify_notification(method, &json!({})), Ok(class));
    }
    for method in [
        "thread/closed",
        "turn/started",
        "turn/completed",
        "item/agentMessage/delta",
        "item/started",
        "item/completed",
        "item/reasoning/summaryTextDelta",
        "item/reasoning/summaryPartAdded",
        "item/reasoning/textDelta",
        "error",
    ] {
        assert_eq!(
            classify_notification(method, &json!({})),
            Ok(NotificationClass::Conversation)
        );
    }
    for method in ["configWarning", "model/rerouted"] {
        assert_eq!(
            classify_notification(method, &json!({})),
            Err(Fault::Policy)
        );
    }
    for method in [
        "future/unknown",
        "tool/call",
        "item/commandExecution/requestApproval",
        "account/gatewayOAuth/changed",
        "thread/environment/connected",
        "thread/settings/updated",
        "thread/deleted",
        "serverRequest/resolved",
    ] {
        assert_eq!(
            classify_notification(method, &json!({})),
            Err(Fault::Unsupported)
        );
    }
    for (method, params, _, _) in fixtures() {
        let request =
            serde_json::to_vec(&json!({"id":99,"method":method,"params":params})).unwrap();
        assert!(
            matches!(decode(&request), Err(Fault::Unsupported)),
            "server request {method}"
        );
    }
}
#[test]
fn startup_allows_only_global_advisories_without_satisfying_the_expected_receipt() {
    for phase in [None, Some(3)] {
        let warning = br#"{"method":"warning","params":{"message":"Synthetic"}}"#;
        assert!(matches!(
            startup(warning, phase),
            Ok(Startup::Informational(AdvisoryKind::Warning))
        ));
        assert!(
            startup(
                br#"{"method":"warning","params":{"threadId":"unowned","message":"Synthetic"}}"#,
                phase
            )
            .is_err()
        );
        assert!(startup(br#"{"method":"modelProvider/authRecoveryCompleted","params":{"threadId":"t","turnId":"u","provider":"openai","message":"Synthetic"}}"#,phase).is_err());
        assert!(matches!(
            startup(
                br#"{"method":"configWarning","params":{"message":"Synthetic"}}"#,
                phase
            ),
            Err(Fault::Policy)
        ));
    }
    assert!(matches!(
        startup(br#"{"id":3,"result":{}}"#, Some(3)),
        Ok(Startup::Reply(_))
    ));
    assert!(startup(br#"{"id":3,"result":{}}"#, Some(4)).is_err());
}
#[test]
fn bounded_numeric_and_collection_shapes_fail_closed_without_quota_projection() {
    for bad in [json!(1.5), json!(2147483648_i64), json!("12"), Value::Null] {
        assert!(
            classify_notification(
                "account/rateLimits/updated",
                &json!({"rateLimits":{"primary":{"usedPercent":bad}}})
            )
            .is_err()
        );
    }
    assert!(
        classify_notification(
            "account/rateLimits/updated",
            &json!({"rateLimits":{"primary":{"usedPercent":0,"resetsAt":1.5}}})
        )
        .is_err()
    );
    assert!(
        classify_notification(
            "model/verification",
            &json!({"threadId":"t","turnId":"u","verifications":[{}]})
        )
        .is_err()
    );
    assert!(
        classify_notification(
            "model/verification",
            &json!({"threadId":"t","turnId":"u","verifications":vec!["x";4097]})
        )
        .is_err()
    );
    assert!(
        classify_notification(
            "warning",
            &json!({"message":"x".repeat(crate::protocol::MAX_REPLY_BYTES+1)})
        )
        .is_err()
    );
    let mut metadata = Value::Null;
    for _ in 0..65 {
        metadata = json!([metadata]);
    }
    assert!(
        classify_notification(
            "turn/moderationMetadata",
            &json!({"threadId":"t","turnId":"u","metadata":metadata})
        )
        .is_err()
    );
}
#[test]
fn duplicate_keys_and_forged_envelopes_never_reach_advisory_routing() {
    for frame in [
        br#"{"method":"warning","params":{"message":"one","message":"two"}}"#.as_slice(),
        br#"{"method":"warning","method":"account/updated","params":{"message":"x"}}"#,
        br#"{"method":"warning","params":{"message":"x"},"id":null}"#,
        br#"{"method":"warning","params":{"message":"x"},"emittedAtMs":"1"}"#,
    ] {
        assert!(decode(frame).is_err());
    }
}
#[test]
fn complete_advisory_sequence_cannot_complete_or_mutate_original_conversation() {
    use crate::conversation::{Conversation, Event, Outcome, Scope};
    let scope = Scope::new("owned".into(), [1; 16], 1).unwrap();
    let mut conversation = Conversation::new(scope.clone());
    conversation
        .begin_start(&scope, 1, super::policy_tests::choice())
        .unwrap();
    conversation.reply(&scope,1,&json!({"thread":{"id":"t","ephemeral":false,"status":{"type":"idle"}},"model":"original-model","modelProvider":"openai","reasoningEffort":"high"})).unwrap();
    conversation.begin_turn(&scope, 2).unwrap();
    conversation
        .reply(
            &scope,
            2,
            &json!({"turn":{"id":"u","status":"inProgress","error":null}}),
        )
        .unwrap();
    let mut events = Vec::new();
    for (method, params, _, _) in fixtures() {
        match classify_notification(method, &params).unwrap() {
            NotificationClass::Informational(_) => (),
            NotificationClass::Conversation => {
                events.extend(conversation.notification(&scope, method, &params).unwrap())
            }
            _ => panic!("unexpected authority"),
        }
    }
    assert!(events.is_empty());
    let params = json!({"threadId":"t","turn":{"id":"u","status":"completed","error":null,"itemsView":"full","items":[]}});
    assert_eq!(
        classify_notification("turn/completed", &params),
        Ok(NotificationClass::Conversation)
    );
    events.extend(
        conversation
            .notification(&scope, "turn/completed", &params)
            .unwrap(),
    );
    assert!(matches!(events.last(),Some(Event::Terminal(r)) if r.outcome==Outcome::Completed));
}
#[test]
fn catalog_requests_and_replies_are_not_replaced_by_advisories() {
    assert_eq!(
        model_list(7, Some("opaque")),
        json!({"id":7,"method":"model/list","params":{"limit":100,"includeHidden":false,"cursor":"opaque"}})
    );
    let Incoming::Reply { id, result } =
        decode(br#"{"id":7,"result":{"data":[],"nextCursor":null}}"#).unwrap()
    else {
        panic!("reply")
    };
    assert_eq!(id, 7);
    assert!(result.unwrap()["data"].is_array());
}
#[test]
fn metadata_cache_and_read_receipts_do_not_restore_tools_or_certify_turns() {
    assert!(matches!(
        startup(br#"{"method":"skills/changed","params":{}}"#, Some(4)),
        Ok(Startup::Informational(AdvisoryKind::SkillsChanged))
    ));
    for first in [
        Value::Null,
        json!({"type":"threadStart"}),
        json!({"type":"turn","turnId":"old-turn"}),
    ] {
        assert_eq!(
            classify_notification(
                "thread/readState/changed",
                &json!({"threadId":"t","readState":{"revision":"opaque","firstUnread":first}})
            ),
            Ok(NotificationClass::Informational(Advisory {
                kind: AdvisoryKind::ThreadReadState,
                scope: AdvisoryScope::Thread("t")
            }))
        );
    }
    for first in [
        json!(false),
        json!({"type":"turn"}),
        json!({"type":"futureAction"}),
    ] {
        assert!(
            classify_notification(
                "thread/readState/changed",
                &json!({"threadId":"t","readState":{"revision":"opaque","firstUnread":first}})
            )
            .is_err()
        );
    }
    assert!(
        classify_notification(
            "thread/readState/changed",
            &json!({"threadId":"t","readState":{"revision":3,"firstUnread":null}})
        )
        .is_err()
    );
    assert!(classify_notification("thread/queue/changed", &json!({})).is_err());
    // The queue hint neither supplies a queued prompt nor starts a turn. These
    // related operations remain unsupported, even with identical parameters.
    for method in [
        "thread/queue/add",
        "thread/queue/resume",
        "skills/execute",
        "skills/import",
        "thread/goal/updated",
    ] {
        assert_eq!(
            classify_notification(method, &json!({"threadId":"t"})),
            Err(Fault::Unsupported)
        );
    }
}

#[test]
fn late_usage_is_a_separate_typed_class_with_the_canonical_snapshot_validator() {
    let counts = json!({"totalTokens":0,"inputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0});
    let mut params = json!({"threadId":"t","turnId":"u","tokenUsage":{"total":counts,"last":counts,"modelContextWindow":null}});
    assert_eq!(
        classify_notification("thread/tokenUsage/updated", &params),
        Ok(NotificationClass::ConversationUsage(AdvisoryScope::Turn {
            thread_id: "t",
            turn_id: "u"
        }))
    );
    params["futurePresentation"] = json!(true);
    assert!(classify_notification("thread/tokenUsage/updated", &params).is_ok());
    for bad in [json!(-1), json!(1.5), json!("0"), Value::Null] {
        let mut wrong = params.clone();
        wrong["tokenUsage"]["last"]["inputTokens"] = bad;
        assert!(classify_notification("thread/tokenUsage/updated", &wrong).is_err());
    }
    for key in ["total", "last", "modelContextWindow"] {
        let mut wrong = params.clone();
        wrong["tokenUsage"].as_object_mut().unwrap().remove(key);
        assert!(classify_notification("thread/tokenUsage/updated", &wrong).is_err());
    }
    params["tokenUsage"]["last"]["cachedInputTokens"] = json!(1);
    assert!(classify_notification("thread/tokenUsage/updated", &params).is_err());
}
