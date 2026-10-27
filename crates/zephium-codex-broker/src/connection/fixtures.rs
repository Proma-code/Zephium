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
    cancel_on_begin: Option<Arc<AtomicBool>>,
    lose_bind_ack: bool,
    lose_begin_ack: bool,
    lose_skip_ack: bool,
    skip_calls: usize,
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
            durable::Call::Open { .. }
            | durable::Call::History { .. }
            | durable::Call::InspectOwner => None,
            durable::Call::AdvanceGrant { expected } => Some(*expected),
            durable::Call::Load { scope, .. }
            | durable::Call::Bind { scope, .. }
            | durable::Call::Skip { scope, .. }
            | durable::Call::Begin { scope, .. }
            | durable::Call::RecordTurn { scope, .. }
            | durable::Call::Settle { scope, .. } => Some(*scope),
        };
        if requested.is_some() && requested != memory.scope {
            return durable::Reply::Failed(durable::Fault::StaleGrant);
        }
        match call {
            durable::Call::InspectOwner => durable::Reply::Owner(memory.scope),
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
                expected_revision,
            } => {
                let prior = memory.records.get(&conversation).cloned();
                let (revision, dispatch) = match (prior, expected_revision) {
                    (None, None) => (1, durable::Dispatch::Ready),
                    (Some(prior), Some(expected))
                        if prior.revision == expected
                            && prior.thread_id.is_none()
                            && matches!(prior.dispatch, durable::Dispatch::Skipped { .. }) =>
                    {
                        (expected + 1, prior.dispatch)
                    }
                    _ => return fail,
                };
                let value = durable::Association {
                    scope,
                    conversation: conversation.clone(),
                    thread_id: Some(thread_id),
                    revision,
                    dispatch,
                };
                memory.records.insert(conversation, value.clone());
                if let Some(cancel) = &memory.cancel_on_bind {
                    cancel.store(true, Ordering::Release);
                }
                if memory.lose_bind_ack {
                    return durable::Reply::Failed(durable::Fault::OutcomeUnknown);
                }
                durable::Reply::Association(Some(value))
            }
            durable::Call::Skip {
                scope,
                conversation,
                expected_revision,
                reply,
            } => {
                memory.skip_calls += 1;
                let prior = memory.records.get(&conversation).cloned();
                let (thread_id, revision) = match (prior, expected_revision) {
                    (None, None) => (None, 1),
                    (Some(prior), Some(expected)) if prior.revision == expected => {
                        let allowed = match prior.dispatch {
                            durable::Dispatch::Pending {
                                reply: pending,
                                turn_id: None,
                            } => pending == reply,
                            durable::Dispatch::Pending { .. } => false,
                            durable::Dispatch::Ready => true,
                            durable::Dispatch::Skipped { reply: old }
                            | durable::Dispatch::Settled { reply: old, .. } => reply > old,
                        };
                        if !allowed {
                            return fail;
                        }
                        (prior.thread_id, expected + 1)
                    }
                    _ => return fail,
                };
                let value = durable::Association {
                    scope,
                    conversation: conversation.clone(),
                    thread_id,
                    revision,
                    dispatch: durable::Dispatch::Skipped { reply },
                };
                memory.records.insert(conversation, value.clone());
                if memory.lose_skip_ack {
                    return durable::Reply::Failed(durable::Fault::OutcomeUnknown);
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
                    || value.thread_id.is_none()
                    || matches!(value.dispatch, durable::Dispatch::Pending { .. })
                    || matches!(value.dispatch,durable::Dispatch::Skipped{reply:prior}|durable::Dispatch::Settled{reply:prior,..} if reply<=prior)
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
                if let Some(cancel) = &memory.cancel_on_begin {
                    cancel.store(true, Ordering::Release);
                }
                if memory.lose_begin_ack {
                    return durable::Reply::Failed(durable::Fault::OutcomeUnknown);
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
        effort: EffortSelection::Explicit(crate::catalog::Effort::High),
        text: match reply {
            1 => "synthetic first $skill_one",
            2 => "synthetic second $skill_two",
            _ => "synthetic third $skill_three",
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
            Scenario::CancelBeforeActor => call_cancel.store(true, Ordering::Release),
            Scenario::CancelBeforeWrite => state.cancel_on_begin = Some(call_cancel.clone()),
            Scenario::LostBindAck => {
                state.lose_bind_ack = true;
                state.cancel_on_bind = Some(call_cancel.clone());
            }
            Scenario::LostBeginAck => {
                state.lose_begin_ack = true;
                state.cancel_on_begin = Some(call_cancel.clone());
            }
            Scenario::LostSkipAck => {
                state.lose_skip_ack = true;
                state.cancel_on_bind = Some(call_cancel.clone());
            }
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
                    thread_id: Some("synthetic-owned-thread".into()),
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
        PreparationBudget::ordinary(Instant::now() + Duration::from_secs(10)).into(),
        &cancel,
    )
    .map_err(|_| "owner-handshake")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut catalog = false;
    let mut started = false;
    let mut second = false;
    let mut third = false;
    let mut cancellation = false;
    let mut retried = false;
    let mut terminals = 0;
    let mut closed = 0;
    let mut invalidated = false;
    let mut guarded_failure = false;
    let mut history_uncertain = false;
    let mut skipped = 0;
    let mut quota_fault = None;
    let skips_success = matches!(
        scenario,
        Scenario::CancelBeforeActor
            | Scenario::CancelDuringPreparation
            | Scenario::CancelBeforeTurn
            | Scenario::CancelBeforeWrite
    );
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
            Err(Fault::ScopeChanged) if matches!(scenario, Scenario::RevisionBeforeWrite) => {
                guarded_failure = true;
                connection
                    .take_failure_events(&scope)
                    .map_err(|_| "owner-failure-events")?
            }
            Err(Fault::PersistenceUnavailable)
                if matches!(
                    scenario,
                    Scenario::LostSettleAck
                        | Scenario::LostStartAck
                        | Scenario::LostBindAck
                        | Scenario::LostBeginAck
                        | Scenario::LostSkipAck
                ) =>
            {
                guarded_failure = true;
                connection
                    .take_failure_events(&scope)
                    .map_err(|_| "owner-failure-events")?
            }
            Err(Fault::Transport) if matches!(scenario, Scenario::LostPreparationAck) => {
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
                Event::Quota(view) => {
                    if let zephium_provider_accounts::quota::QuotaState::Error(fault) = view.state {
                        quota_fault = Some(fault);
                    }
                }
                Event::Catalog(_) if !started => {
                    started = true;
                    if matches!(scenario, Scenario::QuotaExpired) {
                        // Controlled elapsed observation deadline, not a changed
                        // production timeout. Its eventual owned reply is late.
                        let pending = connection
                            .pending
                            .values_mut()
                            .find(|pending| matches!(pending.kind, PendingKind::Quota(_)))
                            .ok_or("quota-pending-fixture")?;
                        pending.deadline = Instant::now();
                    }
                    let mut text = if matches!(scenario, Scenario::ColdRestore) {
                        request(2, false)
                    } else {
                        request(1, true)
                    };
                    if scenario.three_turns() {
                        text.effort = EffortSelection::Default;
                    }
                    text.admission =
                        TextAdmission::from_native(call_cancel.clone(), revision.clone(), 1);
                    connection
                        .start_text(&scope, text, &cancel)
                        .map_err(|_| "owner-first")?;
                    if matches!(scenario, Scenario::CancelDuringPreparation) {
                        call_cancel.store(true, Ordering::Release);
                        connection
                            .cancel_turn(&scope, &cancel)
                            .map_err(|_| "owner-preparation-cancel")?;
                    }
                }
                Event::NeverStarted { reply } => {
                    if !skips_success
                        || reply != 1
                        || skipped != 0
                        || !connection.can_admit_text(&scope)
                    {
                        return Err("owner-false-unsent");
                    }
                    let row = associations
                        .0
                        .lock()
                        .map_err(|_| "owner-store-lock")?
                        .records
                        .get("synthetic-page")
                        .cloned()
                        .ok_or("owner-skip-row")?;
                    if row.dispatch != (durable::Dispatch::Skipped { reply: 1 })
                        || row.thread_id.is_none()
                            != matches!(scenario, Scenario::CancelBeforeActor)
                    {
                        return Err("owner-skip-proof");
                    }
                    skipped += 1;
                    second = true;
                    connection
                        .start_text(&scope, request(2, false), &cancel)
                        .map_err(|_| "owner-after-unsent")?;
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
                        Scenario::TwoTurns
                        | Scenario::QuotaExpired
                        | Scenario::QuotaMalformed
                        | Scenario::QuotaRefused
                        | Scenario::ColdRestore
                        | Scenario::CancelBeforeActor
                        | Scenario::CancelDuringPreparation
                        | Scenario::CancelBeforeTurn
                        | Scenario::CancelBeforeWrite => conversation::Outcome::Completed,
                        Scenario::Cancel => conversation::Outcome::Interrupted,
                        Scenario::Invalidate => conversation::Outcome::Unknown,
                        Scenario::QuotaReplacement
                        | Scenario::QuotaCoalesced
                        | Scenario::RevisionBeforeWrite
                        | Scenario::LostSettleAck
                        | Scenario::LostStartAck
                        | Scenario::LostBindAck
                        | Scenario::LostBeginAck
                        | Scenario::LostSkipAck
                        | Scenario::LostPreparationAck => return Err("owner-unexpected-terminal"),
                    };
                    if result.outcome != expected || result.thread_id != "synthetic-owned-thread" {
                        return Err("owner-terminal");
                    }
                    if matches!(scenario, Scenario::Cancel) && !result.cancel_requested {
                        return Err("owner-cancel-intent");
                    }
                    if (matches!(scenario, Scenario::ColdRestore) || skips_success)
                        && result.final_response() != Some("synthetic second answer")
                    {
                        return Err("owner-cold-text");
                    }
                    if scenario.three_turns()
                        && result.final_response()
                            != Some(match terminals {
                                1 => "synthetic first answer",
                                2 => "synthetic second answer",
                                _ => "synthetic third answer",
                            })
                    {
                        return Err("owner-terminal-text");
                    }
                }
                Event::Conversation(conversation::Event::Closed { .. }) => {
                    closed += 1;
                    if scenario.three_turns() && !second {
                        let mut divergent = request(2, false);
                        divergent.previous_reply = Some(99);
                        if connection.start_text(&scope, divergent, &cancel)
                            != Err(Fault::HistoryDiverged)
                        {
                            return Err("owner-intervening-history");
                        }
                        second = true;
                        let mut next = request(2, false);
                        next.effort = EffortSelection::Explicit(crate::catalog::Effort::None);
                        connection
                            .start_text(&scope, next, &cancel)
                            .map_err(|_| "owner-second")?;
                    } else if scenario.three_turns() && closed == 2 && !third {
                        third = true;
                        let mut next = request(3, false);
                        next.previous_reply = Some(2);
                        next.effort = EffortSelection::Default;
                        connection
                            .start_text(&scope, next, &cancel)
                            .map_err(|_| "owner-third")?;
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
                || skipped != 0
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
        if (skips_success
            && skipped == 1
            && terminals == 1
            && closed
                == if matches!(scenario, Scenario::CancelBeforeActor) {
                    1
                } else {
                    2
                })
            || (scenario.three_turns() && closed == 3)
            || (matches!(scenario, Scenario::Cancel | Scenario::ColdRestore) && closed == 1)
        {
            if (scenario.three_turns() && (terminals != 3 || !retried || !third))
                || (matches!(scenario, Scenario::Cancel | Scenario::ColdRestore) && terminals != 1)
            {
                return Err("owner-counts");
            }
            if matches!(scenario, Scenario::QuotaExpired)
                && quota_fault != Some(zephium_provider_accounts::quota::QuotaFault::TimedOut)
            {
                return Err("quota-expiry-not-observed");
            }
            if matches!(scenario, Scenario::QuotaMalformed)
                && quota_fault
                    != Some(zephium_provider_accounts::quota::QuotaFault::InvalidPercentage)
            {
                return Err("quota-malformation-not-observed");
            }
            if matches!(scenario, Scenario::QuotaRefused)
                && quota_fault
                    != Some(zephium_provider_accounts::quota::QuotaFault::ProviderRefused)
            {
                return Err("quota-refusal-not-observed");
            }
            connection.close().map_err(|_| "owner-transcript-close")?;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let state = associations.0.lock().map_err(|_| "owner-store-lock")?;
    let record = state.records.get("synthetic-page");
    if matches!(scenario, Scenario::LostPreparationAck) {
        if record.is_some() || state.skip_calls != 0 {
            return Err("owner-unknown-preparation");
        }
    } else {
        let record = record.ok_or("owner-store-record")?;
        if scenario.three_turns()
            && (!matches!(record.dispatch, durable::Dispatch::Settled { reply: 3, .. })
                || record.thread_id.as_deref() != Some("synthetic-owned-thread"))
        {
            return Err("owner-three-turn-history");
        }
        if matches!(scenario, Scenario::LostBindAck) {
            if record.dispatch != durable::Dispatch::Ready || state.skip_calls != 0 {
                return Err("owner-unknown-bind");
            }
        } else if matches!(scenario, Scenario::LostSkipAck) {
            if record.dispatch != (durable::Dispatch::Skipped { reply: 1 }) || state.skip_calls != 1
            {
                return Err("owner-unknown-skip");
            }
        } else if matches!(
            scenario,
            Scenario::Invalidate
                | Scenario::RevisionBeforeWrite
                | Scenario::LostSettleAck
                | Scenario::LostStartAck
                | Scenario::LostBeginAck
        ) {
            if !matches!(record.dispatch, durable::Dispatch::Pending { .. })
                || state.skip_calls != 0
            {
                return Err("owner-unknown-journal");
            }
        } else if !matches!(record.dispatch, durable::Dispatch::Settled { .. })
            || (skips_success && state.skip_calls != 1)
        {
            return Err("owner-terminal-journal");
        }
    }
    drop(state);
    Arc::try_unwrap(runtime)
        .map_err(|_| "owner-runtime-retained")?
        .close()?;
    Ok(())
}
pub(crate) fn qualify() -> Result<(), &'static str> {
    for (scenario, marker) in [
        (
            Scenario::CancelBeforeActor,
            "codex-owner-unsent-before-actor-continuity",
        ),
        (
            Scenario::CancelDuringPreparation,
            "codex-owner-unsent-preparation-continuity",
        ),
        (
            Scenario::CancelBeforeWrite,
            "codex-owner-unsent-writer-continuity",
        ),
        (Scenario::LostBindAck, "codex-owner-unsent-bind-ack-unknown"),
        (
            Scenario::LostBeginAck,
            "codex-owner-unsent-begin-ack-unknown",
        ),
        (Scenario::LostSkipAck, "codex-owner-unsent-skip-ack-unknown"),
        (
            Scenario::LostPreparationAck,
            "codex-owner-preparation-ack-unknown",
        ),
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

/// Separate later qualifier; the existing fifteen-owner inventory is unchanged.
pub(crate) fn qualify_quota() -> Result<(), &'static str> {
    quota_replacement(Scenario::QuotaReplacement)?;
    println!("codex-quota-account-replacement-late-reply-ignored: native-code=0");
    quota_replacement(Scenario::QuotaCoalesced)?;
    println!("codex-quota-same-pump-account-replacement-coalesced: native-code=0");
    for (scenario, marker) in [
        (
            Scenario::QuotaRefused,
            "codex-quota-provider-refusal-turn-settled",
        ),
        (
            Scenario::QuotaExpired,
            "codex-quota-expired-read-late-reply-turn-settled",
        ),
        (
            Scenario::QuotaMalformed,
            "codex-quota-malformed-reading-turn-settled",
        ),
    ] {
        run(scenario)?;
        println!("{marker}: native-code=0");
    }
    Ok(())
}

fn quota_replacement(scenario: Scenario) -> Result<(), &'static str> {
    let (session, runtime) = crate::windows::fixtures::owner_session(scenario)?;
    let mut adapter = MemoryAssociations::default();
    let store_scope = durable::Scope {
        incarnation: [8; 16],
        grant_epoch: 1,
    };
    if adapter.call(durable::Call::Open {
        incarnation: store_scope.incarnation,
    }) != durable::Reply::Scope(store_scope)
    {
        return Err("quota-store");
    }
    let scope =
        Scope::new("synthetic-quota-profile".into(), [7; 16], 1).map_err(|_| "quota-scope")?;
    let stop = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut owner = Connection::bootstrap_session(
        scope.clone(),
        session,
        runtime.clone(),
        StoreBinding {
            associations: Box::new(adapter),
            scope: store_scope,
        },
        ConfigProjection::V164Plus,
        PreparationBudget::ordinary(deadline).into(),
        &stop,
    )
    .map_err(|_| "quota-bootstrap")?;
    let mut ready = 0;
    let mut reread = false;
    loop {
        if Instant::now() >= deadline {
            return Err("quota-replacement-timeout");
        }
        let events = owner
            .poll(&scope, &stop)
            .map_err(|_| "quota-replacement-poll")?;
        for event in events {
            match event.into_event() {
                Event::Account(AccountState::ChatGpt) => {
                    ready += 1;
                    if ready == 1 && matches!(scenario, Scenario::QuotaCoalesced) {
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(runtime.temp().join("quota-batch-go"))
                            .map_err(|_| "quota-batch-go")?;
                        while !runtime.temp().join("quota-batch-ready").exists() {
                            if Instant::now() >= deadline {
                                return Err("quota-batch-ready");
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
                Event::Account(AccountState::Unknown) if ready == 1 && !reread => {
                    reread = true;
                    owner
                        .read_account(&scope, &stop)
                        .map_err(|_| "quota-new-account-read")?;
                }
                Event::Quota(view)
                    if matches!(
                        view.state,
                        zephium_provider_accounts::quota::QuotaState::Available(_)
                            | zephium_provider_accounts::quota::QuotaState::Stale { .. }
                    ) =>
                {
                    return Err("quota-old-account-leak")
                }
                _ => (),
            }
        }
        if ready == 2 && owner.pending.is_empty() {
            let view = owner.quota(&scope).map_err(|_| "quota-new-projection")?;
            if view.checking
                || !matches!(
                    view.state,
                    zephium_provider_accounts::quota::QuotaState::Unavailable(_)
                )
            {
                return Err("quota-replaced-projection");
            }
            owner.close().map_err(|_| "quota-replacement-close")?;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Arc::try_unwrap(runtime)
        .map_err(|_| "quota-runtime-retained")?
        .close()
}
