use super::*;
fn definitions() -> Vec<Definition> {
    PageTool::ALL
        .into_iter()
        .map(|tool| Definition {
            name: tool.name().into(),
            description: format!("Owned synthetic {}", tool.name()),
            input_schema: json!({"type":"object","properties":{"value":{"type":"string"}}}),
        })
        .collect()
}
fn manifest() -> NativeManifest {
    NativeManifest::page_v1(definitions()).unwrap()
}
fn binding(manifest: &NativeManifest) -> TurnBinding {
    TurnBinding::new(
        Scope::new("profile".into(), [1; 16], 1).unwrap(),
        1,
        "page".into(),
        1,
        "thread".into(),
        "turn".into(),
        manifest,
    )
    .unwrap()
}
fn bytes(id: Value, call: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"id":id,"method":"item/tool/call","params":{"threadId":"thread","turnId":"turn","callId":call,"namespace":null,"tool":"read_page","arguments":{"value":"owned"}}})).unwrap()
}
fn request(manifest: &NativeManifest, id: i64) -> ToolRequest {
    ToolRequest::decode(&bytes(json!(id), &format!("call-{id}")), manifest).unwrap()
}
#[test]
fn actual_fixed_names_are_exact_once_and_original_specs_are_tagged() {
    let m = manifest();
    assert_eq!(m.wire().as_array().unwrap().len(), 9);
    for value in m.wire().as_array().unwrap() {
        assert_eq!(value["type"], "function");
        assert_eq!(value["deferLoading"], false);
        assert!(value.get("namespace").is_none());
    }
    let mut missing = definitions();
    missing.pop();
    assert!(matches!(
        NativeManifest::page_v1(missing),
        Err(Fault::Manifest)
    ));
    let mut duplicate = definitions();
    duplicate[8].name = duplicate[0].name.clone();
    assert!(matches!(
        NativeManifest::page_v1(duplicate),
        Err(Fault::Manifest)
    ));
    let mut host = definitions();
    host[0].name = "shell".into();
    assert!(matches!(
        NativeManifest::page_v1(host),
        Err(Fault::Manifest)
    ));
}
#[test]
fn canonical_schema_hash_and_instruction_policy_are_separate_and_sensitive() {
    let a = manifest();
    let mut reordered = definitions();
    reordered.reverse();
    let b = NativeManifest::page_v1(reordered).unwrap();
    assert_eq!(a.digest(), b.digest());
    let mut changed = definitions();
    changed[0].description.push('!');
    assert_ne!(
        a.digest(),
        NativeManifest::page_v1(changed).unwrap().digest()
    );
    assert_ne!(
        NativeManifest::instruction_digest("fixed A").unwrap(),
        NativeManifest::instruction_digest("fixed B").unwrap()
    );
    assert!(matches!(
        NativeManifest::instruction_digest(""),
        Err(Fault::Limit)
    ));
}
#[test]
fn exact_server_id_domain_is_not_coerced_into_client_or_call_id() {
    let m = manifest();
    for id in [
        json!(i64::MIN),
        json!(0),
        json!(i64::MAX),
        json!("0"),
        json!(""),
        json!("id\u{0}line\n"),
    ] {
        let b = binding(&m);
        let mut ledger = TurnLedger::new(b.clone());
        ledger
            .admit(ToolRequest::decode(&bytes(id.clone(), "independent-call"), &m).unwrap())
            .unwrap();
        let ticket = ledger.claim(&b).unwrap().unwrap();
        assert_eq!(ticket.ordinal(), 1);
        let effect = ledger.start(ticket, &b).unwrap();
        assert_eq!(effect.ordinal(), 1);
        assert!(effect.tool() == PageTool::ReadPage);
        assert_eq!(effect.arguments(), &json!({"value":"owned"}));
        assert_ne!(effect.body_digest(), [0; 32]);
        let reply = ledger
            .settle(
                effect,
                EffectOutcome::Text {
                    text: "result".into(),
                    success: true,
                },
            )
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_slice(&ledger.handoff(reply, &b).unwrap()).unwrap();
        assert_eq!(value["id"], id);
        assert_eq!(
            value["result"],
            json!({"contentItems":[{"type":"inputText","text":"result"}],"success":true})
        );
    }
    for id in [
        json!(1.5),
        json!(u64::MAX),
        Value::Null,
        json!("x".repeat(257)),
    ] {
        assert!(ToolRequest::decode(&bytes(id, "call"), &m).is_err());
    }
}
#[test]
fn malformed_unoffered_namespaces_and_duplicate_json_never_enter_execution() {
    let m = manifest();
    let base: Value = serde_json::from_slice(&bytes(json!(1), "call")).unwrap();
    for (key, value) in [
        ("tool", json!("shell")),
        ("namespace", json!("browser")),
        ("namespace", json!("functions")),
        ("namespace", json!("custom")),
        ("arguments", json!([])),
    ] {
        let mut v = base.clone();
        v["params"][key] = value;
        assert!(ToolRequest::decode(&serde_json::to_vec(&v).unwrap(), &m).is_err());
    }
    let mut v = base.clone();
    v["method"] = json!("item/commandExecution/requestApproval");
    assert!(ToolRequest::decode(&serde_json::to_vec(&v).unwrap(), &m).is_err());
    let duplicate = String::from_utf8(bytes(json!(1), "call")).unwrap().replace(
        "\"value\":\"owned\"",
        "\"value\":\"owned\",\"value\":\"changed\"",
    );
    assert!(matches!(
        ToolRequest::decode(duplicate.as_bytes(), &m),
        Err(Fault::Envelope)
    ));
}
#[test]
fn arguments_depth_bytes_and_complete_serialized_frame_are_bounded() {
    let m = manifest();
    let mut v: Value = serde_json::from_slice(&bytes(json!(1), "call")).unwrap();
    v["params"]["arguments"] = json!({"value":"x".repeat(MAX_ARGUMENT_BYTES)});
    assert!(matches!(
        ToolRequest::decode(&serde_json::to_vec(&v).unwrap(), &m),
        Err(Fault::Limit)
    ));
    let mut nested = json!(null);
    for _ in 0..34 {
        nested = json!([nested]);
    }
    v["params"]["arguments"] = json!({"value":nested});
    assert!(matches!(
        ToolRequest::decode(&serde_json::to_vec(&v).unwrap(), &m),
        Err(Fault::Limit)
    ));
    assert!(matches!(
        ToolRequest::decode(&vec![b' '; MAX_FRAME_BYTES + 1], &m),
        Err(Fault::Limit)
    ));
}
#[test]
fn exact_call_body_and_native_binding_are_part_of_each_ticket() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b.clone());
    ledger.admit(request(&m, 1)).unwrap();
    let ticket = ledger.claim(&b).unwrap().unwrap();
    assert!(ticket.tool() == PageTool::ReadPage);
    assert_eq!(ticket.arguments(), &json!({"value":"owned"}));
    assert_ne!(ticket.body_digest(), [0; 32]);
    let mut wrong = b.clone();
    wrong.account_epoch += 1;
    assert!(matches!(ledger.start(ticket, &wrong), Err(Fault::Scope)));
    ledger.retire();
    assert!(ledger.local_effects_settled());
    let mut altered = definitions();
    altered[0].description.push('!');
    let other = NativeManifest::page_v1(altered).unwrap();
    let mut ledger = TurnLedger::new(b);
    assert!(matches!(
        ledger.admit(request(&other, 1)),
        Err(Fault::Scope)
    ));
}
#[test]
fn duplicate_rpc_or_call_never_replays_and_string_id_remains_distinct() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b);
    ledger.admit(request(&m, 1)).unwrap();
    assert!(matches!(
        ledger.admit(request(&m, 1)),
        Err(Fault::Duplicate)
    ));
    assert!(matches!(
        ledger.admit(ToolRequest::decode(&bytes(json!(2), "call-1"), &m).unwrap()),
        Err(Fault::Duplicate)
    ));
    ledger
        .admit(ToolRequest::decode(&bytes(json!("1"), "call-string"), &m).unwrap())
        .unwrap();
    assert_eq!(ledger.admitted(), 2);
}
#[test]
fn finite_pre_ack_buffer_requires_exact_eventual_original_turn_and_manifest() {
    let m = manifest();
    let b = binding(&m);
    let mut early = EarlyRequests::new();
    for id in 0..8 {
        early.push(request(&m, id)).unwrap();
    }
    assert!(matches!(early.push(request(&m, 8)), Err(Fault::Limit)));
    let ledger = early.join(b.clone()).unwrap();
    assert_eq!(ledger.admitted(), 8);
    let mut early = EarlyRequests::new();
    early.push(request(&m, 1)).unwrap();
    let mut wrong = b;
    wrong.turn = "foreign".into();
    assert!(matches!(early.join(wrong), Err(Fault::Scope)));
}
#[test]
fn queue_is_bounded_and_parallel_original_requests_execute_serially() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b.clone());
    for id in 0..8 {
        ledger.admit(request(&m, id)).unwrap();
    }
    assert!(matches!(ledger.admit(request(&m, 8)), Err(Fault::Limit)));
    let claimed = ledger.claim(&b).unwrap().unwrap();
    assert!(matches!(ledger.claim(&b), Err(Fault::Busy)));
    let running = ledger.start(claimed, &b).unwrap();
    assert!(matches!(ledger.claim(&b), Err(Fault::Busy)));
    let reply = ledger
        .settle(
            running,
            EffectOutcome::Text {
                text: "done".into(),
                success: true,
            },
        )
        .unwrap()
        .unwrap();
    ledger.handoff(reply, &b).unwrap();
    assert!(ledger.claim(&b).unwrap().is_some());
}
#[test]
fn total_admissions_are_shared_forty_not_reset_after_tool_results() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b.clone());
    for id in 0..40 {
        ledger.admit(request(&m, id)).unwrap();
        let t = ledger.claim(&b).unwrap().unwrap();
        let e = ledger.start(t, &b).unwrap();
        let r = ledger
            .settle(
                e,
                EffectOutcome::Text {
                    text: "ok".into(),
                    success: true,
                },
            )
            .unwrap()
            .unwrap();
        ledger.handoff(r, &b).unwrap();
    }
    assert_eq!(ledger.admitted(), 40);
    assert!(matches!(ledger.admit(request(&m, 40)), Err(Fault::Limit)));
}
#[test]
fn cancellation_before_dispatch_prevents_effect_and_never_fakes_provider_terminal() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b.clone());
    ledger.admit(request(&m, 1)).unwrap();
    let ticket = ledger.claim(&b).unwrap().unwrap();
    ledger.retire();
    assert!(matches!(ledger.start(ticket, &b), Err(Fault::Retired)));
    assert!(ledger.local_effects_settled());
    assert!(matches!(ledger.claim(&b), Err(Fault::Retired)));
}
#[test]
fn terminal_or_cancel_during_effect_keeps_local_owner_until_settlement_no_late_reply() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b.clone());
    ledger.admit(request(&m, 1)).unwrap();
    let t = ledger.claim(&b).unwrap().unwrap();
    let e = ledger.start(t, &b).unwrap();
    ledger.retire();
    assert!(!ledger.local_effects_settled());
    assert!(ledger
        .settle(
            e,
            EffectOutcome::Text {
                text: "effect completed locally".into(),
                success: true
            }
        )
        .unwrap()
        .is_none());
    assert!(ledger.local_effects_settled());
}
#[test]
fn dropped_effect_waiter_is_not_physical_settlement_and_foreign_ledger_cannot_adopt() {
    let m = manifest();
    let b = binding(&m);
    let mut a = TurnLedger::new(b.clone());
    let mut other = TurnLedger::new(b.clone());
    a.admit(request(&m, 1)).unwrap();
    let t = a.claim(&b).unwrap().unwrap();
    assert!(matches!(other.start(t, &b), Err(Fault::Ticket)));
    a.retire();
    let mut a = TurnLedger::new(b.clone());
    a.admit(request(&m, 2)).unwrap();
    let t = a.claim(&b).unwrap().unwrap();
    let e = a.start(t, &b).unwrap();
    drop(e);
    a.retire();
    assert!(!a.local_effects_settled());
}
#[test]
fn response_is_one_shot_bounded_and_retired_after_uncertain_handoff() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b.clone());
    ledger.admit(request(&m, 1)).unwrap();
    let t = ledger.claim(&b).unwrap().unwrap();
    let e = ledger.start(t, &b).unwrap();
    let r = ledger
        .settle(
            e,
            EffectOutcome::Text {
                text: "failure".into(),
                success: false,
            },
        )
        .unwrap()
        .unwrap();
    ledger.retire();
    assert!(matches!(ledger.handoff(r, &b), Err(Fault::Retired)));
    assert!(matches!(ledger.admit(request(&m, 1)), Err(Fault::Retired)));
    let mut ledger = TurnLedger::new(b.clone());
    ledger.admit(request(&m, 2)).unwrap();
    let t = ledger.claim(&b).unwrap().unwrap();
    let e = ledger.start(t, &b).unwrap();
    assert!(matches!(
        ledger.settle(
            e,
            EffectOutcome::Text {
                text: "x".repeat(MAX_RESULT_BYTES + 1),
                success: true
            }
        ),
        Err(Fault::Limit)
    ));
    assert!(ledger.local_effects_settled());
}

#[test]
fn unknown_local_outcome_retires_dispatch_without_fabricating_failure_text() {
    let m = manifest();
    let b = binding(&m);
    let mut ledger = TurnLedger::new(b.clone());
    ledger.admit(request(&m, 1)).unwrap();
    ledger.admit(request(&m, 2)).unwrap();
    let t = ledger.claim(&b).unwrap().unwrap();
    let e = ledger.start(t, &b).unwrap();
    assert!(ledger.settle(e, EffectOutcome::Unknown).unwrap().is_none());
    assert!(matches!(ledger.claim(&b), Err(Fault::Retired)));
    assert!(ledger.local_effects_settled());
}
