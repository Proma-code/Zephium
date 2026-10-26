//! Fresh-only synthetic owner proof. This storage fake is never a product
//! constructor: real SQLite persistence is separately qualified by Store tests.
use super::*;
use crate::connection::fixture_peer::Scenario;
use std::sync::Mutex;
#[derive(Clone, Default)]
pub(crate) struct MemoryAssociations(Arc<Mutex<Memory>>);
#[derive(Default)]
struct Memory {
    scope: Option<durable::Scope>,
    records: BTreeMap<String, durable::Association>,
    cancel_on_bind: Option<Arc<AtomicBool>>,
    revision_on_begin: Option<Arc<AtomicU64>>,
    lose_settle_ack: bool,
    lose_start_ack: bool,
}
impl Associations for MemoryAssociations {
    fn call(&mut self, call: durable::Call) -> durable::Reply {
        let Ok(mut memory) = self.0.lock() else {
            return durable::Reply::Failed(durable::Fault::Unavailable);
        };
        if !call.valid() {
            return durable::Reply::Failed(durable::Fault::Invalid);
        }
        let fail = durable::Reply::Failed(durable::Fault::Conflict);
        let requested = match &call {
            durable::Call::Open { .. } | durable::Call::History { .. } => None,
            durable::Call::AdvanceGrant { expected } => Some(*expected),
            durable::Call::Load { scope, .. }
            | durable::Call::Bind { scope, .. }
            | durable::Call::Begin { scope, .. }
            | durable::Call::RecordTurn { scope, .. }
            | durable::Call::Settle { scope, .. } => Some(*scope),
        };
        if requested.is_some() && requested != memory.scope {
            return durable::Reply::Failed(durable::Fault::StaleGrant);
        }
        match call {
            durable::Call::Open { incarnation } => {
                if let Some(scope) = memory.scope {
                    if scope.incarnation != incarnation {
                        return fail;
                    }
                    durable::Reply::Scope(scope)
                } else {
                    let scope = durable::Scope {
                        incarnation,
                        grant_epoch: 1,
                    };
                    memory.scope = Some(scope);
                    durable::Reply::Scope(scope)
                }
            }
            durable::Call::AdvanceGrant { expected } => {
                let Some(epoch) = expected.grant_epoch.checked_add(1) else {
                    return fail;
                };
                let scope = durable::Scope {
                    incarnation: expected.incarnation,
                    grant_epoch: epoch,
                };
                memory.scope = Some(scope);
                memory.records.clear();
                durable::Reply::Scope(scope)
            }
            durable::Call::History { conversation } => {
                durable::Reply::Association(memory.records.get(&conversation).cloned())
            }
            durable::Call::Load { conversation, .. } => {
                durable::Reply::Association(memory.records.get(&conversation).cloned())
            }
            durable::Call::Bind {
                scope,
                conversation,
                thread_id,
            } => {
                if memory.records.contains_key(&conversation) {
                    return fail;
                }
                let value = durable::Association {
                    scope,
                    conversation: conversation.clone(),
                    thread_id,
                    revision: 1,
                    dispatch: durable::Dispatch::Ready,
                };
                memory.records.insert(conversation, value.clone());
                if let Some(cancel) = &memory.cancel_on_bind {
                    cancel.store(true, Ordering::Release);
                }
                durable::Reply::Association(Some(value))
            }
            durable::Call::Begin {
                conversation,
                expected_revision,
                reply,
                ..
            } => {
                let Some(value) = memory.records.get_mut(&conversation) else {
                    return fail;
                };
                if value.revision != expected_revision
                    || matches!(value.dispatch, durable::Dispatch::Pending { .. })
                    || matches!(value.dispatch,durable::Dispatch::Settled{reply:prior,..} if reply<=prior)
                {
                    return fail;
                }
                value.revision += 1;
                value.dispatch = durable::Dispatch::Pending {
                    reply,
                    turn_id: None,
                };
                let result = durable::Reply::Association(Some(value.clone()));
                if let Some(revision) = &memory.revision_on_begin {
                    revision.fetch_add(1, Ordering::AcqRel);
                }
                result
            }
            durable::Call::RecordTurn {
                conversation,
                expected_revision,
                reply,
                turn_id,
                ..
            } => {
                if memory.lose_start_ack {
                    return durable::Reply::Failed(durable::Fault::OutcomeUnknown);
                }
                let Some(value) = memory.records.get_mut(&conversation) else {
                    return fail;
                };
                if value.revision != expected_revision
                    || value.dispatch
                        != (durable::Dispatch::Pending {
                            reply,
                            turn_id: None,
                        })
                {
                    return fail;
                }
                value.revision += 1;
                value.dispatch = durable::Dispatch::Pending {
                    reply,
                    turn_id: Some(turn_id),
                };
                durable::Reply::Association(Some(value.clone()))
            }
            durable::Call::Settle {
                conversation,
                expected_revision,
                reply,
                turn_id,
                outcome,
                ..
            } => {
                if memory.lose_settle_ack {
                    return durable::Reply::Failed(durable::Fault::OutcomeUnknown);
                }
                let Some(value) = memory.records.get_mut(&conversation) else {
                    return fail;
                };
                if value.revision != expected_revision
                    || !matches!(&value.dispatch,durable::Dispatch::Pending{reply:pending,turn_id:known} if *pending==reply&&known.as_ref().is_none_or(|id|id==&turn_id))
                {
                    return fail;
                }
                value.revision += 1;
                value.dispatch = durable::Dispatch::Settled {
                    reply,
                    turn_id,
                    outcome,
                };
                durable::Reply::Association(Some(value.clone()))
            }
        }
    }
}
fn request(reply: u64, new_conversation: bool) -> TextRequest {
    TextRequest {
        admission: TextAdmission::from_native(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(1)),
            1,
        ),
        reply,
        previous_reply: if new_conversation { None } else { Some(1) },
        key: ConversationKey::from_native("synthetic-page".into()).unwrap(),
        new_conversation,
        model: "synthetic-picker".into(),
        effort: Effort::High,
        text: if reply == 1 {
            "synthetic first $skill_one"
        } else {
            "synthetic second $skill_two"
        }
        .into(),
    }
}
fn run(scenario: Scenario) -> Result<(), &'static str> {
    let (session, runtime) = crate::windows::fixtures::owner_session(scenario)?;
    let associations = MemoryAssociations::default();
    let call_cancel = Arc::new(AtomicBool::new(false));
    let revision = Arc::new(AtomicU64::new(1));
    {
        let mut state = associations.0.lock().map_err(|_| "owner-store-lock")?;
        match scenario {
            Scenario::CancelBeforeTurn => state.cancel_on_bind = Some(call_cancel.clone()),
            Scenario::RevisionBeforeWrite => state.revision_on_begin = Some(revision.clone()),
            Scenario::LostSettleAck => state.lose_settle_ack = true,
            Scenario::LostStartAck => state.lose_start_ack = true,
            _ => {}
        }
    }
    let scope = Scope::new("synthetic-profile-incarnation".into(), [7; 16], 1)
        .map_err(|_| "owner-scope")?;
    let association_scope = durable::Scope {
        incarnation: [8; 16],
        grant_epoch: 1,
    };
    let mut adapter = associations.clone();
    if adapter.call(durable::Call::Open {
        incarnation: association_scope.incarnation,
    }) != durable::Reply::Scope(association_scope)
    {
        return Err("owner-store");
    }
    if matches!(scenario, Scenario::ColdRestore) {
        // Only a native Store receipt restores authority. No renderer/provider ID
        // enters the owner. This fake covers the join, not SQLite durability.
        associations
            .0
            .lock()
            .map_err(|_| "owner-store-lock")?
            .records
            .insert(
                "synthetic-page".into(),
                durable::Association {
                    scope: association_scope,
                    conversation: "synthetic-page".into(),
                    thread_id: "synthetic-owned-thread".into(),
                    revision: 4,
                    dispatch: durable::Dispatch::Settled {
                        reply: 1,
                        turn_id: "turn-one".into(),
                        outcome: durable::Terminal::Completed,
                    },
                },
            );
    }
    let cancel = AtomicBool::new(false);
    let mut connection = Connection::bootstrap_session(
        scope.clone(),
        session,
        Arc::clone(&runtime),
        StoreBinding {
            associations: Box::new(adapter),
            scope: association_scope,
        },
        ConfigProjection::V164Plus,
        Instant::now() + Duration::from_secs(10),
        &cancel,
    )
    .map_err(|_| "owner-handshake")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut catalog = false;
    let mut started = false;
    let mut second = false;
    let mut cancellation = false;
    let mut retried = false;
    let mut terminals = 0;
    let mut closed = 0;
    let mut invalidated = false;
    let mut guarded_failure = false;
    let mut history_uncertain = false;
    loop {
        if Instant::now() >= deadline {
            return Err("owner-transcript-timeout");
        }
        let events = match connection.poll(&scope, &cancel) {
            Ok(events) => events,
            Err(Fault::NotConnected) if matches!(scenario, Scenario::Invalidate) => {
                invalidated = true;
                connection
                    .take_failure_events(&scope)
                    .map_err(|_| "owner-failure-events")?
            }
            Err(Fault::Cancelled) if matches!(scenario, Scenario::CancelBeforeTurn) => {
                guarded_failure = true;
                connection
                    .take_failure_events(&scope)
                    .map_err(|_| "owner-failure-events")?
            }
            Err(Fault::ScopeChanged) if matches!(scenario, Scenario::RevisionBeforeWrite) => {
                guarded_failure = true;
                connection
                    .take_failure_events(&scope)
                    .map_err(|_| "owner-failure-events")?
            }
            Err(Fault::PersistenceUnavailable)
                if matches!(scenario, Scenario::LostSettleAck | Scenario::LostStartAck) =>
            {
                guarded_failure = true;
                connection
                    .take_failure_events(&scope)
                    .map_err(|_| "owner-failure-events")?
            }
            Err(_) => return Err("owner-transcript-poll"),
        };
        for event in events {
            if event.scope() != &scope {
                return Err("owner-event-scope");
            }
            match event.into_event() {
                Event::Account(AccountState::ChatGpt) if !catalog => {
                    catalog = true;
                    connection
                        .refresh_catalog(&scope, &cancel)
                        .map_err(|_| "owner-model-list")?;
                }
                Event::Catalog(_) if !started => {
                    started = true;
                    let mut text = if matches!(scenario, Scenario::ColdRestore) {
                        request(2, false)
                    } else {
                        request(1, true)
                    };
                    text.admission =
                        TextAdmission::from_native(call_cancel.clone(), revision.clone(), 1);
                    connection
                        .start_text(&scope, text, &cancel)
                        .map_err(|_| "owner-first")?;
                }
                Event::Conversation(conversation::Event::TurnStarted { .. })
                    if matches!(scenario, Scenario::Cancel) && !cancellation =>
                {
                    cancellation = true;
                    connection
                        .cancel_turn(&scope, &cancel)
                        .map_err(|_| "owner-interrupt")?;
                }
                Event::Conversation(conversation::Event::ProviderError { will_retry: true }) => {
                    retried = true
                }
                Event::Conversation(conversation::Event::Terminal(result)) => {
                    terminals += 1;
                    let expected = match scenario {
                        Scenario::TwoTurns | Scenario::ColdRestore => {
                            conversation::Outcome::Completed
                        }
                        Scenario::Cancel => conversation::Outcome::Interrupted,
                        Scenario::Invalidate => conversation::Outcome::Unknown,
                        Scenario::CancelBeforeTurn
                        | Scenario::RevisionBeforeWrite
                        | Scenario::LostSettleAck
                        | Scenario::LostStartAck => return Err("owner-unexpected-terminal"),
                    };
                    if result.outcome != expected || result.thread_id != "synthetic-owned-thread" {
                        return Err("owner-terminal");
                    }
                    if matches!(scenario, Scenario::Cancel) && !result.cancel_requested {
                        return Err("owner-cancel-intent");
                    }
                    if matches!(scenario, Scenario::ColdRestore)
                        && result.final_response() != Some("synthetic second answer")
                    {
                        return Err("owner-cold-text");
                    }
                    if matches!(scenario, Scenario::TwoTurns)
                        && result.final_response()
                            != Some(if terminals == 1 {
                                "synthetic first answer"
                            } else {
                                "synthetic second answer"
                            })
                    {
                        return Err("owner-terminal-text");
                    }
                }
                Event::Conversation(conversation::Event::Closed { .. }) => {
                    closed += 1;
                    if matches!(scenario, Scenario::TwoTurns) && !second {
                        let mut divergent = request(2, false);
                        divergent.previous_reply = Some(99);
                        if connection.start_text(&scope, divergent, &cancel)
                            != Err(Fault::HistoryDiverged)
                        {
                            return Err("owner-intervening-history");
                        }
                        second = true;
                        connection
                            .start_text(&scope, request(2, false), &cancel)
                            .map_err(|_| "owner-second")?;
                    }
                }
                Event::HistoryUncertain(result)
                    if matches!(scenario, Scenario::LostSettleAck | Scenario::LostStartAck) =>
                {
                    if history_uncertain
                        || result.outcome != conversation::Outcome::Completed
                        || result.final_response() != Some("synthetic preserved answer")
                    {
                        return Err("owner-known-terminal-lost");
                    }
                    history_uncertain = true;
                }
                Event::OpenBrowser(_) => return Err("owner-unexpected-login"),
                _ => {}
            }
        }
        if guarded_failure {
            if terminals != 0
                || (matches!(scenario, Scenario::LostSettleAck | Scenario::LostStartAck)
                    && !history_uncertain)
                || connection.start_text(&scope, request(2, false), &cancel) != Err(Fault::Closed)
            {
                return Err("owner-retired-replay");
            }
            drop(connection);
            break;
        }
        if invalidated {
            if terminals != 1
                || connection.start_text(&scope, request(2, false), &cancel) != Err(Fault::Closed)
            {
                return Err("owner-invalidated-replay");
            }
            drop(connection);
            break;
        }
        if (matches!(scenario, Scenario::TwoTurns) && closed == 2)
            || (matches!(scenario, Scenario::Cancel | Scenario::ColdRestore) && closed == 1)
        {
            if (matches!(scenario, Scenario::TwoTurns) && (terminals != 2 || !retried))
                || (matches!(scenario, Scenario::Cancel | Scenario::ColdRestore) && terminals != 1)
            {
                return Err("owner-counts");
            }
            connection.close().map_err(|_| "owner-transcript-close")?;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let record = associations
        .0
        .lock()
        .map_err(|_| "owner-store-lock")?
        .records
        .get("synthetic-page")
        .cloned()
        .ok_or("owner-store-record")?;
    if matches!(scenario, Scenario::CancelBeforeTurn) {
        if record.dispatch != durable::Dispatch::Ready {
            return Err("owner-cancel-before-begin");
        }
    } else if matches!(
        scenario,
        Scenario::Invalidate
            | Scenario::RevisionBeforeWrite
            | Scenario::LostSettleAck
            | Scenario::LostStartAck
    ) {
        if !matches!(record.dispatch, durable::Dispatch::Pending { .. }) {
            return Err("owner-unknown-journal");
        }
    } else if !matches!(record.dispatch, durable::Dispatch::Settled { .. }) {
        return Err("owner-terminal-journal");
    }
    Arc::try_unwrap(runtime)
        .map_err(|_| "owner-runtime-retained")?
        .close()?;
    Ok(())
}
pub(crate) fn qualify() -> Result<(), &'static str> {
    for (scenario, marker) in [
        (Scenario::TwoTurns, "codex-owner-two-turns-cold-resume"),
        (
            Scenario::ColdRestore,
            "codex-owner-stored-thread-cold-resume",
        ),
        (Scenario::Cancel, "codex-owner-cancel-terminal"),
        (Scenario::Invalidate, "codex-owner-invalidation-no-replay"),
        (Scenario::CancelBeforeTurn, "codex-owner-cancel-before-turn"),
        (
            Scenario::RevisionBeforeWrite,
            "codex-owner-revision-before-write",
        ),
        (
            Scenario::LostSettleAck,
            "codex-owner-known-terminal-lost-ack",
        ),
        (
            Scenario::LostStartAck,
            "codex-owner-early-terminal-lost-ack",
        ),
    ] {
        run(scenario)?;
        println!("{marker}: native-code=0");
    }
    Ok(())
}
