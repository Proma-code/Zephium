use super::*;
use crate::{browser_port::*, conversation::Scope, dynamic_tools::PageTool};
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex, time::Duration};

struct State {
    input: Option<BrowserInput>,
    calls: Vec<(u8, String, serde_json::Value)>,
    keys: VecDeque<BrowserResultKey>,
    results: VecDeque<BrowserResult>,
    completion: Option<BrowserCompletionKey>,
    local: Option<BrowserLocalReceipt>,
    lost_result: bool,
    lost_local: bool,
    refuse: bool,
    closed: bool,
    revokes: Vec<BrowserRevokeReason>,
    deadline: Instant,
}
struct Port(Arc<Mutex<State>>);
struct Budget(Arc<Mutex<State>>);
impl BrowserBudget for Budget {
    fn deadline(&self) -> Result<Instant, BrowserPortFault> {
        Ok(self.0.lock().unwrap().deadline)
    }
    fn hard_deadline(&self) -> Result<Instant, BrowserPortFault> {
        self.deadline()
    }
}
impl BrowserPort for Port {
    fn poll_input(&mut self) -> BrowserInputPoll {
        self.0
            .lock()
            .unwrap()
            .input
            .take()
            .map(BrowserInputPoll::Ready)
            .unwrap_or(BrowserInputPoll::Pending)
    }
    fn dispatch(&mut self, call: BrowserDispatch) -> BrowserDispatchOutcome {
        let mut state = self.0.lock().unwrap();
        assert!(!state.closed);
        if state.refuse {
            return BrowserDispatchOutcome::Refused(call);
        }
        let (ordinal, name, args, key) = call.into_parts();
        state.calls.push((ordinal, name.into(), args));
        state.keys.push_back(key);
        BrowserDispatchOutcome::Accepted
    }
    fn poll_result(&mut self) -> BrowserResultPoll {
        let mut state = self.0.lock().unwrap();
        if let Some(result) = state.results.pop_front() {
            BrowserResultPoll::Ready(result)
        } else if state.lost_result {
            state.lost_result = false;
            BrowserResultPoll::Lost
        } else {
            BrowserResultPoll::Pending
        }
    }
    fn budget(&self) -> Arc<dyn BrowserBudget> {
        Arc::new(Budget(self.0.clone()))
    }
    fn revoke(&mut self, reason: BrowserRevokeReason) {
        self.0.lock().unwrap().revokes.push(reason);
    }
    fn close_admission(&mut self, completion: BrowserCompletionKey) {
        let mut state = self.0.lock().unwrap();
        assert!(!state.closed);
        state.closed = true;
        state.completion = Some(completion);
    }
    fn poll_local_settlement(&mut self) -> BrowserLocalSettlement {
        let mut state = self.0.lock().unwrap();
        if let Some(local) = state.local.take() {
            BrowserLocalSettlement::Observed(local)
        } else if state.lost_local {
            BrowserLocalSettlement::Lost
        } else {
            BrowserLocalSettlement::Pending
        }
    }
}
fn fixture() -> (BrowserOwner, Arc<Mutex<State>>, TurnBinding) {
    let manifest = Arc::new(
        BrowserManifest::from_native_page_v1(
            "fixed instructions".into(),
            PageTool::ALL
                .into_iter()
                .map(|tool| BrowserToolDefinition {
                    name: tool.name().into(),
                    description: "original Page definition".into(),
                    schema: json!({"type":"object"}),
                })
                .collect(),
        )
        .unwrap_or_else(|_| panic!()),
    );
    let binding = TurnBinding::new(
        Scope::new("profile".into(), [1; 16], 1).unwrap(),
        1,
        "page".into(),
        1,
        "thread".into(),
        "turn".into(),
        manifest.tools(),
    )
    .unwrap();
    let state = Arc::new(Mutex::new(State {
        input: None,
        calls: Vec::new(),
        keys: VecDeque::new(),
        results: VecDeque::new(),
        completion: None,
        local: None,
        lost_result: false,
        lost_local: false,
        refuse: false,
        closed: false,
        revokes: Vec::new(),
        deadline: Instant::now() + Duration::from_secs(90),
    }));
    (
        BrowserOwner::new(manifest, Box::new(Port(state.clone()))),
        state,
        binding,
    )
}
fn request(id: i64) -> Vec<u8> {
    serde_json::to_vec(&json!({"id":id,"method":"item/tool/call","params":{"threadId":"thread","turnId":"turn","callId":format!("call-{id}"),"namespace":null,"tool":"read_page","arguments":{"value":id}}})).unwrap()
}
fn result(state: &Arc<Mutex<State>>, text: &str) {
    let mut state = state.lock().unwrap();
    let key = state.keys.pop_front().unwrap();
    state
        .results
        .push_back(key.complete(text.into(), true).ok().unwrap());
}
fn actual_local_receipt(state: &Arc<Mutex<State>>) {
    let mut state = state.lock().unwrap();
    state.local = Some(state.completion.take().unwrap().observed());
}
#[test]
fn early_requests_wait_actual_turn_join_and_page_effects_are_serial() {
    let (mut owner, state, binding) = fixture();
    owner.request(&request(1)).unwrap();
    owner.request(&request(2)).unwrap();
    assert!(state.lock().unwrap().calls.is_empty());
    owner.bind(binding.clone()).unwrap();
    owner.dispatch_one(&binding, Instant::now()).unwrap();
    owner.dispatch_one(&binding, Instant::now()).unwrap();
    assert_eq!(state.lock().unwrap().calls.len(), 1);
    result(&state, "actual first output");
    owner.poll_results().unwrap();
    owner.dispatch_one(&binding, Instant::now()).unwrap();
    assert_eq!(state.lock().unwrap().calls.len(), 1);
    let response: serde_json::Value = serde_json::from_slice(
        &owner
            .take_response(&binding, Instant::now())
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["result"]["contentItems"][0]["text"],
        "actual first output"
    );
    assert!(owner
        .take_response(&binding, Instant::now())
        .unwrap()
        .is_none());
    owner.dispatch_one(&binding, Instant::now()).unwrap();
    assert_eq!(state.lock().unwrap().calls.len(), 2);
}
#[test]
fn provider_terminal_then_scope_revocation_keeps_entered_effect_until_real_dual_join() {
    let (mut owner, state, binding) = fixture();
    owner.bind(binding.clone()).unwrap();
    owner.request(&request(1)).unwrap();
    owner.dispatch_one(&binding, Instant::now()).unwrap();
    // The Connection's authentic terminal/text is independent of these local
    // transitions. Terminal closes the sender; same-pump authority loss revokes
    // pending Page work, without manufacturing a different provider receipt.
    owner.close_admission(None).unwrap();
    assert!(state.lock().unwrap().revokes.is_empty());
    owner
        .close_admission(Some(BrowserRevokeReason::ScopeChanged))
        .unwrap();
    assert_eq!(
        state.lock().unwrap().revokes,
        vec![BrowserRevokeReason::ScopeChanged]
    );
    owner.original_settled().unwrap();
    assert!(owner.draining());
    assert!(owner.poll_local_settlement().unwrap().is_none());
    result(&state, "actual effect returned after revocation");
    owner.poll_results().unwrap();
    assert!(owner
        .take_response(&binding, Instant::now())
        .unwrap()
        .is_none());
    assert!(owner.poll_local_settlement().unwrap().is_none());
    actual_local_receipt(&state);
    assert_eq!(
        owner.poll_local_settlement().unwrap(),
        Some(LocalOutcome::Observed)
    );
    assert!(!owner.draining());
    owner
        .close_admission(Some(BrowserRevokeReason::Cancelled))
        .unwrap();
    assert_eq!(state.lock().unwrap().revokes.len(), 1);
    assert!(owner.poll_local_settlement().unwrap().is_none());
}
#[test]
fn local_receipt_alone_never_replaces_original_closed_barrier() {
    let (mut owner, state, _) = fixture();
    owner.close_admission(None).unwrap();
    actual_local_receipt(&state);
    assert!(owner.poll_local_settlement().unwrap().is_none());
    owner.original_settled().unwrap();
    assert_eq!(
        owner.poll_local_settlement().unwrap(),
        Some(LocalOutcome::Observed)
    );
}
#[test]
fn lost_individual_result_requires_actual_aggregate_before_unobserved_disposition() {
    let (mut owner, state, binding) = fixture();
    owner.bind(binding.clone()).unwrap();
    owner.request(&request(1)).unwrap();
    owner.dispatch_one(&binding, Instant::now()).unwrap();
    state.lock().unwrap().lost_result = true;
    assert_eq!(owner.poll_results(), Err(Fault::ResultLost));
    owner
        .close_admission(Some(BrowserRevokeReason::ConnectionLost))
        .unwrap();
    owner.original_settled().unwrap();
    assert!(owner.poll_local_settlement().unwrap().is_none());
    actual_local_receipt(&state);
    assert_eq!(
        owner.poll_local_settlement().unwrap(),
        Some(LocalOutcome::ResultUnobserved)
    );
}
#[test]
fn lost_aggregate_and_foreign_bridge_receipt_remain_quarantined() {
    let (mut owner, state, _) = fixture();
    owner
        .close_admission(Some(BrowserRevokeReason::Cancelled))
        .unwrap();
    owner.original_settled().unwrap();
    state.lock().unwrap().lost_local = true;
    assert_eq!(owner.poll_local_settlement(), Err(Fault::LocalReceiptLost));
    assert!(owner.poll_local_settlement().unwrap().is_none());
    assert!(owner.draining());
    let (mut other, other_state, _) = fixture();
    other.close_admission(None).unwrap();
    other.original_settled().unwrap();
    other_state.lock().unwrap().local =
        Some(state.lock().unwrap().completion.take().unwrap().observed());
    assert_eq!(other.poll_local_settlement(), Err(Fault::Protocol));
    assert!(other.draining());
}
#[test]
fn refused_channel_call_is_not_in_accepted_set_and_does_not_execute() {
    let (mut owner, state, binding) = fixture();
    owner.bind(binding.clone()).unwrap();
    owner.request(&request(1)).unwrap();
    state.lock().unwrap().refuse = true;
    assert_eq!(
        owner.dispatch_one(&binding, Instant::now()),
        Err(Fault::PageRefused)
    );
    assert!(owner.accepted.is_empty() && state.lock().unwrap().calls.is_empty());
    owner
        .close_admission(Some(BrowserRevokeReason::Protocol))
        .unwrap();
    owner.original_settled().unwrap();
    actual_local_receipt(&state);
    assert_eq!(
        owner.poll_local_settlement().unwrap(),
        Some(LocalOutcome::Observed)
    );
}
#[test]
fn deadline_is_rechecked_after_human_wait_ends_and_before_each_dispatch() {
    let (mut owner, state, binding) = fixture();
    owner.bind(binding.clone()).unwrap();
    owner.request(&request(1)).unwrap();
    state.lock().unwrap().deadline = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        owner.dispatch_one(&binding, Instant::now()),
        Err(Fault::Budget)
    );
    assert!(state.lock().unwrap().calls.is_empty());
}
#[test]
fn one_input_and_closed_admission_never_accept_another_request() {
    let (mut owner, state, binding) = fixture();
    assert!(owner.poll_input(Instant::now()).unwrap().is_none());
    state.lock().unwrap().input = Some(
        BrowserInput::from_native_page("whole composed context".into())
            .unwrap_or_else(|_| panic!()),
    );
    assert!(owner.poll_input(Instant::now()).unwrap().is_some());
    assert!(matches!(
        owner.poll_input(Instant::now()),
        Err(Fault::Revoked)
    ));
    assert_eq!(owner.manifest().instructions(), "fixed instructions");
    owner.bind(binding.clone()).unwrap();
    assert!(owner.binding() == Some(&binding));
    owner
        .close_admission(Some(BrowserRevokeReason::Cancelled))
        .unwrap();
    assert_eq!(owner.request(&request(1)), Err(Fault::Revoked));
    assert_eq!(
        owner.dispatch_one(&binding, Instant::now()),
        Err(Fault::Revoked)
    );
}

#[test]
fn terminal_projection_removes_only_owned_dynamic_items_without_settling_effects() {
    let (mut owner, state, binding) = fixture();
    owner.request(&request(1)).unwrap();
    owner.bind(binding.clone()).unwrap();
    owner.dispatch_one(&binding, Instant::now()).unwrap();
    let dynamic = json!({"id":"call-1","type":"dynamicToolCall","tool":"read_page","arguments":{"value":1},"status":"inProgress","contentItems":null,"success":null,"durationMs":null});
    let text =
        json!({"id":"final","type":"agentMessage","text":"exact final","phase":"final_answer"});
    let mut params = json!({"threadId":"thread","turn":{"id":"turn","status":"completed","error":null,"itemsView":"full","items":[dynamic,text]}});
    owner.project_terminal(&mut params).unwrap();
    assert_eq!(params["turn"]["items"], json!([text]));
    owner.close_admission(None).unwrap();
    owner.original_settled().unwrap();
    assert_eq!(owner.poll_local_settlement().unwrap(), None);
    assert_eq!(state.lock().unwrap().calls.len(), 1);
}
#[test]
fn terminal_projection_rejects_unbound_changed_duplicate_and_cross_kind_ids() {
    for changed in 0..6 {
        let (mut owner, _, binding) = fixture();
        owner.request(&request(1)).unwrap();
        owner.bind(binding).unwrap();
        let mut item = json!({"id":"call-1","type":"dynamicToolCall","tool":"read_page","arguments":{"value":1},"status":"completed"});
        let mut items = vec![item.clone()];
        match changed {
            0 => items[0]["id"] = json!("unknown"),
            1 => items[0]["arguments"] = json!({"value":"other"}),
            2 => items.push(item.clone()),
            3 => items.push(json!({"id":"call-1","type":"agentMessage","text":"x"})),
            4 => {
                item["tool"] = json!("shell");
                items[0] = item;
            }
            _ => items = vec![item; crate::conversation::MAX_ITEMS + 1],
        };
        let mut params = json!({"threadId":"thread","turn":{"id":"turn","status":"completed","error":null,"itemsView":"full","items":items}});
        assert!(owner.project_terminal(&mut params).is_err());
    }
}

#[test]
fn actual_owner_lifecycle_and_live_budget_views_are_observational() {
    let (mut owner, _, binding) = fixture();
    let item = json!({"id":"call-1","type":"dynamicToolCall","tool":"read_page","arguments":{"value":1},"status":"inProgress"});
    let params = json!({"threadId":"thread","turnId":"turn","item":item,"startedAtMs":1});
    assert_eq!(owner.lifecycle("item/started", &params), Ok(true));
    owner.request(&request(1)).unwrap();
    owner.bind(binding).unwrap();
    assert!(!owner.local_receipt_lost());
    assert_eq!(
        owner.budget().hard_deadline().unwrap(),
        owner.hard_deadline().unwrap()
    );
    assert_eq!(
        owner.late_lifecycle("item/started", &params, "thread", "turn"),
        Ok(true)
    );
    assert!(owner
        .late_lifecycle("item/started", &params, "thread", "other")
        .is_err());
}

#[test]
fn tool_and_text_item_ids_cannot_collide_across_notifications_or_terminal_snapshot() {
    let (mut owner, _, binding) = fixture();
    owner
        .observe_item_identity(
            "item/agentMessage/delta",
            &json!({"threadId":"thread","turnId":"turn","itemId":"call-1","delta":"x"}),
        )
        .unwrap();
    assert!(owner.request(&request(1)).is_err());
    let (mut owner, _, _) = fixture();
    owner.request(&request(1)).unwrap();
    owner.bind(binding).unwrap();
    assert!(owner
        .observe_item_identity(
            "item/agentMessage/delta",
            &json!({"threadId":"thread","turnId":"turn","itemId":"call-1","delta":"x"})
        )
        .is_err());
    let mut terminal = json!({"threadId":"thread","turn":{"id":"turn","status":"completed","error":null,"itemsView":"summary","items":[{"id":"call-1","type":"agentMessage","text":"wrong kind"}]}});
    assert!(owner.project_terminal(&mut terminal).is_err());
}

#[test]
fn terminal_then_local_receipt_then_revocation_still_forwards_until_dual_delivery() {
    let (mut owner, state, _) = fixture();
    owner.close_admission(None).unwrap();
    actual_local_receipt(&state);
    assert!(owner.poll_local_settlement().unwrap().is_none());
    owner
        .close_admission(Some(BrowserRevokeReason::Cancelled))
        .unwrap();
    owner
        .close_admission(Some(BrowserRevokeReason::ScopeChanged))
        .unwrap();
    assert_eq!(
        state.lock().unwrap().revokes,
        vec![
            BrowserRevokeReason::Cancelled,
            BrowserRevokeReason::ScopeChanged
        ]
    );
    assert!(state.lock().unwrap().closed);
    assert!(state.lock().unwrap().completion.is_none());
    owner.original_settled().unwrap();
    assert_eq!(
        owner.poll_local_settlement().unwrap(),
        Some(LocalOutcome::Observed)
    );
    owner
        .close_admission(Some(BrowserRevokeReason::Protocol))
        .unwrap();
    assert_eq!(state.lock().unwrap().revokes.len(), 2);
    assert!(owner.poll_local_settlement().unwrap().is_none());
}

#[test]
fn dual_settlement_survives_later_same_pump_failure_exactly_once() {
    use crate::{
        connection_events::{self, ConversationKey, Event, ScopedEvent},
        conversation,
    };
    let (mut owner, state, binding) = fixture();
    owner.bind(binding.clone()).unwrap();
    owner.close_admission(None).unwrap();
    owner.original_settled().unwrap();
    assert!(owner.poll_local_settlement().unwrap().is_none());
    let scope = Scope::new("profile".into(), [1; 16], 1).unwrap();
    let key = ConversationKey("page".into());
    let scoped = |event| ScopedEvent {
        scope: scope.clone(),
        conversation: Some(key.clone()),
        event,
    };
    // The existing reducer independently qualifies terminal truth. This batch
    // models its emitted fact followed by the actual dual-settlement receipt.
    let terminal = conversation::TurnResult {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        outcome: conversation::Outcome::Completed,
        cancel_requested: false,
        messages: vec![conversation::Message {
            id: "answer".into(),
            text: "original final".into(),
            phase: conversation::MessagePhase::FinalAnswer,
        }],
        usage: None,
    };
    actual_local_receipt(&state);
    let local = owner.poll_local_settlement().unwrap().unwrap();
    let observed = vec![
        scoped(Event::Conversation(conversation::Event::Terminal(terminal))),
        scoped(Event::BrowserSettled { reply: 1, local }),
        scoped(Event::Account(
            zephium_provider_accounts::codex::AccountState::ChatGpt,
        )),
        scoped(Event::Quota(
            crate::quota_owner::QuotaOwner::new(scope.clone(), 1).view(),
        )),
    ];
    // A later quota send fails after BrowserOwner was consumed. Apply the EXACT
    // production failure filter, not another test-only receipt whitelist.
    let mut failure = vec![scoped(Event::Conversation(
        conversation::Event::TransportLost {
            thread_id: Some("thread".into()),
            turn_id: Some("turn".into()),
        },
    ))];
    connection_events::preserve_failure(observed, &mut failure);
    let mut released = 0;
    let mut saw_terminal = false;
    assert_eq!(released, 0);
    for event in std::mem::take(&mut failure) {
        assert!(event.scope() == &scope);
        assert_eq!(event.conversation().unwrap().as_str(), "page");
        match event.into_event() {
            Event::Conversation(conversation::Event::Terminal(result)) => {
                assert_eq!(result.final_response(), Some("original final"));
                saw_terminal = true;
            }
            Event::BrowserSettled {
                reply: 1,
                local: LocalOutcome::Observed,
            } => {
                assert!(saw_terminal);
                released += 1;
            }
            Event::Conversation(conversation::Event::TransportLost { .. }) => {
                assert_eq!(released, 1)
            }
            _ => panic!("failed pump must not publish account/quota readiness"),
        }
    }
    assert_eq!(released, 1);
    assert!(failure.is_empty());
    assert!(owner.poll_local_settlement().unwrap().is_none());
}
