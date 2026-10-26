use super::*;
use crate::catalog::{CatalogBuilder, Effort, EffortSelection};
use serde_json::json;

fn scope() -> Scope {
    Scope::new("native-profile".into(), [1; 16], 1).unwrap()
}
fn choice() -> ModelChoice {
    let mut catalog = CatalogBuilder::new();
    catalog.push_page(None,&json!({"data":[{"id":"picker-id","model":"wire-model","displayName":"Original Name",
        "description":"","hidden":false,"isDefault":true,"inputModalities":["text"],"defaultReasoningEffort":"high",
        "supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}]}],"nextCursor":null})).unwrap();
    catalog
        .finish()
        .unwrap()
        .select("picker-id", EffortSelection::Explicit(Effort::High))
        .unwrap()
}
fn thread_reply() -> Value {
    json!({"thread":{"id":"original-thread","ephemeral":false,"status":{"type":"idle"},"turns":[],"environments":[]},
        "model":"wire-model","modelProvider":"openai","reasoningEffort":"high"})
}
fn turn_reply() -> Value {
    json!({"turn":{"id":"original-turn","status":"inProgress","items":[],"itemsView":"notLoaded","error":null}})
}
fn message(id: &str, text: &str, phase: Value) -> Value {
    json!({"type":"agentMessage","id":id,"text":text,"phase":phase,"memoryCitation":null,"delivery":null,"questions":null})
}
fn terminal(items: Vec<Value>, view: &str, status: &str) -> Value {
    json!({"threadId":"original-thread","turn":{"id":"original-turn","status":status,"items":items,"itemsView":view,"error":null}})
}
fn ready() -> Conversation {
    let mut c = Conversation::new(scope());
    assert!(matches!(
        c.begin_start(&scope(), 1, choice()).unwrap(),
        Command::StartThread { .. }
    ));
    assert_eq!(
        c.reply(&scope(), 1, &thread_reply()).unwrap(),
        vec![Event::ThreadReady {
            thread_id: "original-thread".into(),
            generation: 1
        }]
    );
    c
}
fn running() -> Conversation {
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    assert_eq!(
        c.reply(&scope(), 2, &turn_reply()).unwrap(),
        vec![Event::TurnStarted {
            turn_id: "original-turn".into()
        }]
    );
    c
}
fn notify(c: &mut Conversation, method: &str, value: Value) -> Result<Vec<Event>, Fault> {
    c.notification(&scope(), method, &value)
}
fn final_result(events: Vec<Event>) -> TurnResult {
    events
        .into_iter()
        .find_map(|e| match e {
            Event::Terminal(r) => Some(r),
            _ => None,
        })
        .expect("terminal receipt")
}

#[test]
fn pinned_item_lifecycle_and_summary_terminal_do_not_duplicate_text() {
    let mut c = running();
    let start = message("message", "", json!("final_answer"));
    notify(
        &mut c,
        "item/started",
        json!({"threadId":"original-thread","turnId":"original-turn","item":start,"startedAtMs":1}),
    )
    .unwrap();
    assert_eq!(notify(&mut c,"item/agentMessage/delta",json!({"threadId":"original-thread","turnId":"original-turn","itemId":"message","delta":"Hello"})).unwrap(),vec![Event::Delta{item_id:"message".into(),text:"Hello".into()}]);
    let item = message("message", "Hello", json!("final_answer"));
    assert!(matches!(&notify(&mut c,"item/completed",json!({"threadId":"original-thread","turnId":"original-turn","item":item,"completedAtMs":2})).unwrap()[0],Event::ItemCompleted(_)));
    let events = notify(
        &mut c,
        "turn/completed",
        terminal(vec![item], "summary", "completed"),
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    let result = final_result(events);
    assert_eq!(result.outcome, Outcome::Completed);
    assert_eq!(result.final_response(), Some("Hello"));
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.usage, None);
    assert!(!result.cancel_requested);
    assert_eq!(c.phase(), Phase::Ready);
    // Neither an acknowledgement nor completion authorizes another warm turn.
    assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
}

#[test]
fn not_loaded_terminal_uses_original_completed_items_optional_usage_is_not_zero() {
    let mut c = running();
    for item in [
        message("commentary", "Thinking", json!("commentary")),
        message("legacy", "Answer", Value::Null),
    ] {
        notify(&mut c,"item/completed",json!({"threadId":"original-thread","turnId":"original-turn","item":item,"completedAtMs":1})).unwrap();
    }
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(vec![], "notLoaded", "completed"),
        )
        .unwrap(),
    );
    assert_eq!(result.final_response(), Some("Answer"));
    assert_eq!(result.usage, None);
}

#[test]
fn final_phase_preferred_over_later_unknown_and_commentary_is_not_final() {
    let mut c = running();
    let items = vec![
        message("one", "Final", json!("final_answer")),
        message("two", "Unknown", Value::Null),
        message("three", "Comment", json!("commentary")),
    ];
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(items, "full", "completed"),
        )
        .unwrap(),
    );
    assert_eq!(result.final_response(), Some("Final"));
    let mut c = running();
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(
                vec![message("one", "Comment", json!("commentary"))],
                "summary",
                "completed",
            ),
        )
        .unwrap(),
    );
    assert_eq!(result.final_response(), None);
}

#[test]
fn exact_cold_barrier_preserves_thread_id_and_requires_both_actual_close_and_ack() {
    for close_first in [false, true] {
        let mut c = ready();
        c.begin_unsubscribe(&scope(), 2).unwrap();
        assert_eq!(c.begin_resume(&scope(), 3, choice()), Err(Fault::State));
        if close_first {
            assert!(notify(
                &mut c,
                "thread/closed",
                json!({"threadId":"original-thread"})
            )
            .unwrap()
            .is_empty());
            assert_eq!(c.phase(), Phase::Closing);
            assert!(matches!(
                &c.reply(&scope(), 2, &json!({"status":"unsubscribed"}))
                    .unwrap()[0],
                Event::Closed { generation: 1, .. }
            ));
        } else {
            assert!(c
                .reply(&scope(), 2, &json!({"status":"notLoaded"}))
                .unwrap()
                .is_empty());
            assert_eq!(c.phase(), Phase::Closing);
            assert!(matches!(
                &notify(
                    &mut c,
                    "thread/closed",
                    json!({"threadId":"original-thread"})
                )
                .unwrap()[0],
                Event::Closed { generation: 1, .. }
            ));
        }
        assert_eq!(
            c.begin_resume(&scope(), 3, choice()).unwrap(),
            Command::ResumeThread {
                request_id: 3,
                thread_id: "original-thread".into(),
                choice: choice()
            }
        );
        assert_eq!(
            c.reply(&scope(), 3, &thread_reply()).unwrap(),
            vec![Event::ThreadReady {
                thread_id: "original-thread".into(),
                generation: 2
            }]
        );
        assert!(c.begin_turn(&scope(), 4).is_ok());
    }
}

#[test]
fn same_id_resume_rejects_replacement_model_effort_and_thread() {
    for field in ["model", "reasoningEffort", "thread"] {
        let mut c = ready();
        c.begin_unsubscribe(&scope(), 2).unwrap();
        c.reply(&scope(), 2, &json!({"status":"unsubscribed"}))
            .unwrap();
        notify(
            &mut c,
            "thread/closed",
            json!({"threadId":"original-thread"}),
        )
        .unwrap();
        c.begin_resume(&scope(), 3, choice()).unwrap();
        let mut reply = thread_reply();
        match field {
            "thread" => reply["thread"]["id"] = json!("replacement"),
            other => reply[other] = json!("different"),
        }
        assert_eq!(c.reply(&scope(), 3, &reply), Err(Fault::Protocol));
        assert_eq!(c.phase(), Phase::Failed);
    }
}

#[test]
fn before_response_notifications_are_buffered_then_bound_to_actual_returned_turn() {
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    assert!(notify(
        &mut c,
        "turn/started",
        json!({"threadId":"original-thread","turn":turn_reply()["turn"]})
    )
    .unwrap()
    .is_empty());
    let item = message("message", "Hello", json!("final_answer"));
    assert!(notify(
        &mut c,
        "item/completed",
        json!({"threadId":"original-thread","turnId":"original-turn","item":item,"completedAtMs":1})
    )
    .unwrap()
    .is_empty());
    assert!(notify(
        &mut c,
        "turn/completed",
        terminal(vec![item], "summary", "completed")
    )
    .unwrap()
    .is_empty());
    let result = final_result(c.reply(&scope(), 2, &turn_reply()).unwrap());
    assert_eq!(result.final_response(), Some("Hello"));
}

#[test]
fn stale_native_scope_thread_and_turn_never_publish_or_poison_new_state() {
    let mut c = running();
    let other = Scope::new("native-profile".into(), [1; 16], 2).unwrap();
    assert_eq!(
        c.notification(
            &other,
            "turn/completed",
            &terminal(vec![], "notLoaded", "completed")
        ),
        Err(Fault::ScopeChanged)
    );
    let mut stale = terminal(vec![], "notLoaded", "completed");
    stale["threadId"] = json!("other-thread");
    assert!(notify(&mut c, "turn/completed", stale).unwrap().is_empty());
    let mut stale = terminal(vec![], "notLoaded", "completed");
    stale["turn"]["id"] = json!("old-turn");
    assert!(notify(&mut c, "turn/completed", stale).unwrap().is_empty());
    assert_eq!(c.phase(), Phase::Running);
}

#[test]
fn interrupted_ack_is_not_completion_and_cancel_suppresses_future_output() {
    let mut c = running();
    assert_eq!(
        c.cancel(&scope()).unwrap(),
        vec![Event::InterruptRequired {
            thread_id: "original-thread".into(),
            turn_id: "original-turn".into()
        }]
    );
    assert!(matches!(
        c.begin_interrupt(&scope(), 3).unwrap(),
        Command::Interrupt { .. }
    ));
    assert!(c.reply(&scope(), 3, &json!({})).unwrap().is_empty());
    assert_eq!(c.phase(), Phase::Running);
    assert_eq!(c.begin_interrupt(&scope(), 4), Err(Fault::State));
    assert!(notify(&mut c,"item/agentMessage/delta",json!({"threadId":"original-thread","turnId":"original-turn","itemId":"message","delta":"hidden"})).unwrap().is_empty());
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(vec![], "notLoaded", "interrupted"),
        )
        .unwrap(),
    );
    assert_eq!(result.outcome, Outcome::Interrupted);
    assert!(result.cancel_requested);
}

#[test]
fn cancellation_during_pending_start_waits_for_original_id_without_resend() {
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    assert!(c.cancel(&scope()).unwrap().is_empty());
    let events = c.reply(&scope(), 2, &turn_reply()).unwrap();
    assert_eq!(
        events.last(),
        Some(&Event::InterruptRequired {
            thread_id: "original-thread".into(),
            turn_id: "original-turn".into()
        })
    );
    c.begin_interrupt(&scope(), 3).unwrap();
    // Actual completed status remains truthful even if cancellation raced it.
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(vec![], "notLoaded", "completed"),
        )
        .unwrap(),
    );
    assert_eq!(result.outcome, Outcome::Completed);
    assert!(result.cancel_requested);
}

#[test]
fn eof_and_transport_cancel_do_not_certify_completion_or_allow_resend() {
    let mut c = running();
    let events = c.transport_lost(&scope()).unwrap();
    assert!(matches!(events[0], Event::TransportLost { .. }));
    assert_eq!(final_result(events).outcome, Outcome::Unknown);
    assert_eq!(c.phase(), Phase::Failed);
    assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    assert_eq!(
        c.transport_lost(&scope()).unwrap(),
        vec![Event::TransportLost {
            thread_id: Some("original-thread".into()),
            turn_id: None
        }]
    );
    assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
}

#[test]
fn every_tool_item_server_request_and_action_notification_is_closed() {
    for kind in [
        "commandExecution",
        "fileChange",
        "dynamicToolCall",
        "mcpToolCall",
        "functionCallOutput",
        "collabAgentToolCall",
        "subAgentActivity",
        "webSearch",
        "imageView",
        "imageGeneration",
        "hookPrompt",
        "plan",
        "unknown",
    ] {
        let mut c = running();
        assert_eq!(
            notify(
                &mut c,
                "item/started",
                json!({"threadId":"original-thread","turnId":"original-turn","item":{"type":kind,"id":"action"},"startedAtMs":1})
            ),
            Err(Fault::UnsupportedAction)
        );
        assert_eq!(c.phase(), Phase::Failed);
        assert!(matches!(
            c.begin_interrupt(&scope(), 3).unwrap(),
            Command::Interrupt { .. }
        ));
        assert!(notify(
            &mut c,
            "turn/completed",
            terminal(vec![], "notLoaded", "completed")
        )
        .unwrap()
        .is_empty());
    }
    let mut c = running();
    assert_eq!(c.server_request(&scope()), Err(Fault::UnsupportedAction));
    let mut c = running();
    assert_eq!(
        notify(
            &mut c,
            "turn/plan/updated",
            json!({"threadId":"original-thread","turnId":"original-turn","plan":[]})
        ),
        Err(Fault::UnsupportedAction)
    );
}

#[test]
fn original_retry_telemetry_waits_for_real_terminal_without_broker_resend() {
    for (will_retry, status, outcome) in [
        (true, "completed", Outcome::Completed),
        (false, "failed", Outcome::Failed),
    ] {
        let mut c = running();
        let events = notify(
            &mut c,
            "error",
            json!({
                "threadId":"original-thread", "turnId":"original-turn",
                "willRetry":will_retry, "error":{"message":"synthetic private value"}
            }),
        )
        .unwrap();
        assert_eq!(events, vec![Event::ProviderError { will_retry }]);
        assert!(!format!("{events:?}").contains("private"));
        assert_eq!(c.phase(), Phase::Running);
        assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
        let result = final_result(
            notify(
                &mut c,
                "turn/completed",
                terminal(vec![], "notLoaded", status),
            )
            .unwrap(),
        );
        assert_eq!(result.outcome, outcome);
    }
}

#[test]
fn retry_telemetry_never_certifies_eof_or_expired_transport_as_success() {
    for will_retry in [false, true] {
        let mut c = running();
        notify(
            &mut c,
            "error",
            json!({
                "threadId":"original-thread", "turnId":"original-turn",
                "willRetry":will_retry, "error":{"message":"synthetic private value"}
            }),
        )
        .unwrap();
        // The actor closes transport on its unchanged absolute deadline too.
        let result = final_result(c.transport_lost(&scope()).unwrap());
        assert_eq!(result.outcome, Outcome::Unknown);
        assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
    }
}

#[test]
fn malformed_error_telemetry_and_rpc_errors_remain_terminal() {
    for value in [json!(null), json!("true")] {
        let mut c = running();
        assert_eq!(
            notify(
                &mut c,
                "error",
                json!({
                    "threadId":"original-thread", "turnId":"original-turn",
                    "willRetry":value, "error":{"message":"private"}
                })
            ),
            Err(Fault::Protocol)
        );
    }
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    assert_eq!(c.request_failed(&scope(), 2), Err(Fault::Upstream));
    assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
}

#[test]
fn counters_are_optional_original_snapshots_not_remaining_quota_or_turn_cost() {
    let mut c = running();
    let zero = json!({"totalTokens":0,"inputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0});
    notify(&mut c,"thread/tokenUsage/updated",json!({"threadId":"original-thread","turnId":"original-turn","tokenUsage":{"total":zero,"last":zero,"modelContextWindow":null}})).unwrap();
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(vec![], "notLoaded", "completed"),
        )
        .unwrap(),
    );
    assert_eq!(result.usage.unwrap().last.total_tokens, 0);
    let mut bad = zero;
    bad["inputTokens"] = json!(-1);
    assert_eq!(usage(&bad), Err(Fault::Protocol));
}

#[test]
fn completed_item_is_authoritative_and_delta_only_item_is_not_invented_final() {
    let mut c = running();
    notify(&mut c,"item/agentMessage/delta",json!({"threadId":"original-thread","turnId":"original-turn","itemId":"message","delta":"provisional"})).unwrap();
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(
                vec![message("message", "authoritative", Value::Null)],
                "summary",
                "completed",
            ),
        )
        .unwrap(),
    );
    assert_eq!(result.final_response(), Some("authoritative"));
    let mut c = running();
    notify(&mut c,"item/agentMessage/delta",json!({"threadId":"original-thread","turnId":"original-turn","itemId":"message","delta":"partial"})).unwrap();
    let result = final_result(
        notify(
            &mut c,
            "turn/completed",
            terminal(vec![], "notLoaded", "completed"),
        )
        .unwrap(),
    );
    assert_eq!(result.outcome, Outcome::Completed);
    assert!(result.messages.is_empty());
    assert_eq!(result.final_response(), None);
}

#[test]
fn conflicting_completed_items_and_completed_status_with_error_are_not_receipts() {
    let mut c = running();
    let value = json!({"threadId":"original-thread","turnId":"original-turn","item":message("message","original",Value::Null),"completedAtMs":1});
    notify(&mut c, "item/completed", value).unwrap();
    assert_eq!(
        notify(
            &mut c,
            "turn/completed",
            terminal(
                vec![message("message", "conflict", Value::Null)],
                "summary",
                "completed"
            )
        ),
        Err(Fault::Protocol)
    );
    let mut c = running();
    let mut value = terminal(vec![], "notLoaded", "completed");
    value["turn"]["error"] = json!({"message":"error"});
    assert_eq!(
        notify(&mut c, "turn/completed", value),
        Err(Fault::Protocol)
    );
}

#[test]
fn bounds_and_stale_closure_fail_closed_without_invented_new_thread() {
    let mut c = running();
    assert_eq!(
        notify(
            &mut c,
            "thread/closed",
            json!({"threadId":"original-thread"})
        ),
        Err(Fault::OutcomeUnknown)
    );
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    for _ in 0..MAX_EARLY_EVENTS {
        notify(
            &mut c,
            "turn/started",
            json!({"threadId":"original-thread","turn":turn_reply()["turn"]}),
        )
        .unwrap();
    }
    assert_eq!(
        notify(
            &mut c,
            "turn/started",
            json!({"threadId":"original-thread","turn":turn_reply()["turn"]})
        ),
        Err(Fault::Limit)
    );
    let mut c = running();
    assert_eq!(
        notify(
            &mut c,
            "item/agentMessage/delta",
            json!({"threadId":"original-thread","turnId":"original-turn","itemId":"message","delta":"x".repeat(MAX_TEXT+1)})
        ),
        Err(Fault::Limit)
    );
}

#[test]
fn only_known_deferred_same_id_resume_can_retry_within_three_total_attempts() {
    let mut c = Conversation::new(scope());
    c.begin_start(&scope(), 1, choice()).unwrap();
    assert_eq!(c.resume_deferred(&scope(), 1), Err(Fault::State));
    c.reply(&scope(), 1, &thread_reply()).unwrap();
    c.begin_unsubscribe(&scope(), 2).unwrap();
    c.reply(&scope(), 2, &json!({"status":"unsubscribed"}))
        .unwrap();
    assert_eq!(c.resume_deferred(&scope(), 2), Err(Fault::State));
    assert_eq!(c.begin_resume(&scope(), 3, choice()), Err(Fault::State));
    notify(
        &mut c,
        "thread/closed",
        json!({"threadId":"original-thread"}),
    )
    .unwrap();
    for id in [3, 4] {
        c.begin_resume(&scope(), id, choice()).unwrap();
        assert_eq!(c.resume_deferred(&scope(), id + 100), Err(Fault::State));
        c.resume_deferred(&scope(), id).unwrap();
        assert_eq!(c.phase(), Phase::Closed);
        assert_eq!(c.thread_id(), Some("original-thread"));
    }
    c.begin_resume(&scope(), 5, choice()).unwrap();
    assert_eq!(c.resume_deferred(&scope(), 5), Err(Fault::Limit));
    assert_eq!(c.phase(), Phase::Failed);
    assert_eq!(c.begin_resume(&scope(), 6, choice()), Err(Fault::State));
}

#[test]
fn prepared_text_and_fresh_deny_configuration_keep_same_original_conversation() {
    let first = crate::text_policy::prepare_text("Hello $alpha".into()).unwrap();
    let first_config = first.fixed_config();
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    assert_eq!(
        first.into_turn_input(),
        json!([{"type":"text","text":"Hello $alpha","text_elements":[]} ])
    );
    c.reply(&scope(), 2, &turn_reply()).unwrap();
    notify(
        &mut c,
        "turn/completed",
        terminal(
            vec![message("message", "First answer", json!("final_answer"))],
            "summary",
            "completed",
        ),
    )
    .unwrap();
    let second = crate::text_policy::prepare_text("Now $beta".into()).unwrap();
    assert_ne!(first_config, second.fixed_config());
    c.begin_unsubscribe(&scope(), 3).unwrap();
    c.reply(&scope(), 3, &json!({"status":"unsubscribed"}))
        .unwrap();
    assert_eq!(c.begin_turn(&scope(), 4), Err(Fault::State));
    notify(
        &mut c,
        "thread/closed",
        json!({"threadId":"original-thread"}),
    )
    .unwrap();
    assert!(
        matches!(c.begin_resume(&scope(),4,choice()).unwrap(),Command::ResumeThread{thread_id,..} if thread_id=="original-thread")
    );
    c.reply(&scope(), 4, &thread_reply()).unwrap();
    c.begin_turn(&scope(), 5).unwrap();
    assert_eq!(
        second.into_turn_input(),
        json!([{"type":"text","text":"Now $beta","text_elements":[]} ])
    );
    let mut next_turn = turn_reply();
    next_turn["turn"]["id"] = json!("second-original-turn");
    c.reply(&scope(), 5, &next_turn).unwrap();
    // An old completion cannot finish the next generation's turn.
    assert!(notify(
        &mut c,
        "turn/completed",
        terminal(vec![], "notLoaded", "completed")
    )
    .unwrap()
    .is_empty());
    assert_eq!(c.phase(), Phase::Running);
    assert_eq!(c.thread_id(), Some("original-thread"));
}

#[test]
fn request_ids_and_required_turn_identity_are_not_weakly_correlated() {
    let mut c = ready();
    assert_eq!(c.begin_turn(&scope(), 1), Err(Fault::RequestId));
    c.begin_turn(&scope(), 2).unwrap();
    assert_eq!(c.reply(&scope(), 99, &turn_reply()), Err(Fault::RequestId));
    c.reply(&scope(), 2, &turn_reply()).unwrap();
    assert_eq!(c.reply(&scope(), 2, &turn_reply()), Err(Fault::RequestId));
    assert_eq!(
        notify(
            &mut c,
            "turn/completed",
            json!({"threadId":"original-thread","turn":{"status":"completed"}})
        ),
        Err(Fault::Protocol)
    );
}

#[test]
fn native_cold_store_restore_requires_same_id_resume_and_never_fabricates_receipts() {
    for id in [String::new(), "bad\nthread".into(), "x".repeat(257)] {
        assert!(matches!(
            Conversation::from_cold_store(scope(), id),
            Err(Fault::InvalidInput)
        ));
    }
    let mut c = Conversation::from_cold_store(scope(), "original-thread".into()).unwrap();
    assert_eq!(c.phase(), Phase::Closed);
    assert_eq!(c.active_turn_id(), None);
    assert!(!c.close_seen && !c.unsubscribe_ack);
    assert_eq!(c.begin_start(&scope(), 1, choice()), Err(Fault::State));
    assert_eq!(c.begin_turn(&scope(), 1), Err(Fault::State));
    assert!(matches!(c.begin_resume(&scope(), 1, choice()).unwrap(),
        Command::ResumeThread { thread_id, .. } if thread_id == "original-thread"));
    // A new process has not observed the special same-process closing race.
    assert_eq!(c.resume_deferred(&scope(), 1), Err(Fault::State));
    assert_eq!(
        c.reply(&scope(), 1, &thread_reply()).unwrap(),
        vec![Event::ThreadReady {
            thread_id: "original-thread".into(),
            generation: 1,
        }]
    );
    c.begin_turn(&scope(), 2).unwrap();
    c.reply(&scope(), 2, &turn_reply()).unwrap();
    notify(
        &mut c,
        "turn/completed",
        terminal(vec![], "notLoaded", "completed"),
    )
    .unwrap();
    assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
    assert_eq!(c.begin_resume(&scope(), 3, choice()), Err(Fault::State));
    c.begin_unsubscribe(&scope(), 3).unwrap();
    c.reply(&scope(), 3, &json!({"status":"unsubscribed"}))
        .unwrap();
    assert_eq!(c.begin_resume(&scope(), 4, choice()), Err(Fault::State));
    notify(
        &mut c,
        "thread/closed",
        json!({"threadId":"original-thread"}),
    )
    .unwrap();
    c.begin_resume(&scope(), 4, choice()).unwrap();
}

#[test]
fn native_cold_restore_never_accepts_a_replacement_provider_thread() {
    let mut c = Conversation::from_cold_store(scope(), "stored-thread".into()).unwrap();
    c.begin_resume(&scope(), 1, choice()).unwrap();
    assert_eq!(c.reply(&scope(), 1, &thread_reply()), Err(Fault::Protocol));
    assert_eq!(c.phase(), Phase::Failed);
}

#[test]
fn never_started_transition_requires_exact_pending_rpc_and_real_cold_close() {
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    assert_eq!(c.abandon_unstarted(&scope(), 3), Err(Fault::State));
    c.abandon_unstarted(&scope(), 2).unwrap();
    assert_eq!(c.phase(), Phase::Ready);
    assert!(c.active_turn_id().is_none());
    assert_eq!(c.begin_turn(&scope(), 3), Err(Fault::State));
    c.begin_unsubscribe(&scope(), 3).unwrap();
    assert!(c
        .reply(&scope(), 3, &json!({"status":"unsubscribed"}))
        .unwrap()
        .is_empty());
    assert_eq!(c.phase(), Phase::Closing);
    let events = notify(
        &mut c,
        "thread/closed",
        json!({"threadId":"original-thread"}),
    )
    .unwrap();
    assert!(matches!(events.as_slice(), [Event::Closed { .. }]));
    assert_eq!(c.phase(), Phase::Closed);
    assert!(c.begin_resume(&scope(), 4, choice()).is_ok());
}
#[test]
fn never_started_never_erases_observed_or_early_original_turn_evidence() {
    let mut c = running();
    assert_eq!(c.abandon_unstarted(&scope(), 2), Err(Fault::State));
    assert_eq!(c.active_turn_id(), Some("original-turn"));
    let mut early = ready();
    early.begin_turn(&scope(), 2).unwrap();
    notify(
        &mut early,
        "turn/started",
        json!({"threadId":"original-thread","turn":{"id":"original-turn","status":"inProgress"}}),
    )
    .unwrap();
    assert_eq!(early.abandon_unstarted(&scope(), 2), Err(Fault::State));
    assert!(early.reply(&scope(), 2, &turn_reply()).is_ok());
    assert_eq!(early.active_turn_id(), Some("original-turn"));
}
#[test]
fn never_started_scope_mismatch_does_not_change_current_obligation() {
    let mut c = ready();
    c.begin_turn(&scope(), 2).unwrap();
    let wrong = Scope::new("other".into(), [9; 16], 1).unwrap();
    assert_eq!(c.abandon_unstarted(&wrong, 2), Err(Fault::ScopeChanged));
    assert_eq!(c.phase(), Phase::StartingTurn);
    assert!(c.reply(&scope(), 2, &turn_reply()).is_ok());
}

#[test]
fn default_readback_is_nullable_but_explicit_none_requires_literal() {
    for selection in [
        EffortSelection::Default,
        EffortSelection::Explicit(Effort::None),
    ] {
        for (actual, valid) in [
            (Some(Value::Null), selection == EffortSelection::Default),
            (
                Some(json!("none")),
                selection == EffortSelection::Explicit(Effort::None),
            ),
            (Some(json!("high")), selection == EffortSelection::Default),
            (None, false),
        ] {
            let mut reducer = Conversation::new(scope());
            let selected = crate::effort_policy::tests::choice(selection);
            reducer.begin_start(&scope(), 1, selected).unwrap();
            let mut result = thread_reply();
            result["model"] = json!("original-model");
            if let Some(actual) = actual {
                result["reasoningEffort"] = actual;
            } else {
                result.as_object_mut().unwrap().remove("reasoningEffort");
            }
            assert_eq!(reducer.reply(&scope(), 1, &result).is_ok(), valid);
        }
    }
}

#[test]
fn explicit_none_then_cold_default_keeps_same_thread_and_requires_fresh_nullable_receipt() {
    let mut reducer = Conversation::new(scope());
    let explicit = crate::effort_policy::tests::choice(EffortSelection::Explicit(Effort::None));
    let default = crate::effort_policy::tests::choice(EffortSelection::Default);
    reducer.begin_start(&scope(), 1, explicit).unwrap();
    let mut first = thread_reply();
    first["model"] = json!("original-model");
    first["reasoningEffort"] = json!("none");
    reducer.reply(&scope(), 1, &first).unwrap();
    reducer.begin_turn(&scope(), 2).unwrap();
    reducer.reply(&scope(), 2, &turn_reply()).unwrap();
    notify(
        &mut reducer,
        "turn/completed",
        terminal(
            vec![message("first", "synthetic", json!("final_answer"))],
            "full",
            "completed",
        ),
    )
    .unwrap();
    reducer.begin_unsubscribe(&scope(), 3).unwrap();
    reducer
        .reply(&scope(), 3, &json!({"status":"unsubscribed"}))
        .unwrap();
    notify(
        &mut reducer,
        "thread/closed",
        json!({"threadId":"original-thread"}),
    )
    .unwrap();
    let command = reducer.begin_resume(&scope(), 4, default.clone()).unwrap();
    assert_eq!(
        command,
        Command::ResumeThread {
            request_id: 4,
            thread_id: "original-thread".into(),
            choice: default
        }
    );
    let mut fresh = first;
    fresh["reasoningEffort"] = Value::Null;
    reducer.reply(&scope(), 4, &fresh).unwrap();
    assert!(
        matches!(reducer.begin_turn(&scope(),5).unwrap(), Command::StartTurn {choice,..} if choice.selection() == EffortSelection::Default)
    );
}
