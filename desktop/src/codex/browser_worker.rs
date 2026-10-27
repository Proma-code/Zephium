//! Same-worker Browser turn handoff and factual local drain. No second queue,
//! provider retry, guessed Skip, or local tool execution lives here.
use super::*;
use zephium_codex_broker::{
    browser_port::BrowserRevokeReason,
    connection::{BrowserQuarantine, RejectedBrowser, ScopedEvent},
};

/// Irrecoverable receipt loss retains exact native custody without a hot worker.
/// No methods here can execute, reopen, retry, clean up, or release a turn.
pub(super) struct Quarantine {
    _original: Option<BrowserQuarantine>,
    _rejected: Option<RejectedBrowser>,
}

pub(super) struct Rejected {
    owner: RejectedBrowser,
    call: ReplyCall,
    fault: PageAssistNativeFaultV1,
}

fn park(service: &Service, original: Option<BrowserQuarantine>, rejected: Option<Rejected>) {
    let (rejected, waiter) = match rejected {
        Some(Rejected { owner, call, fault }) => (Some(owner), Some((call, fault))),
        None => (None, None),
    };
    service
        .browser_quarantine
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Quarantine {
            _original: original,
            _rejected: rejected,
        });
    // Explicit quarantine is already installed. No provider outcome is known
    // for an unadopted call; this error is not a local settlement or Skip.
    if let Some((call, fault)) = waiter {
        let _ = call.result.send(ReplyDelivery::Failed(fault));
    }
}

pub(super) fn start(
    service: &Arc<Service>,
    connection: &mut Connection,
    scope: &conversation::Scope,
    call: ReplyCall,
    start: browser_reservation::Start,
    active: &mut Option<Active>,
    rejected: &mut Option<Rejected>,
) -> Result<(), Fault> {
    let owns_slot = service.settling.lock().is_ok_and(|held| {
        held.as_ref().is_some_and(|held| {
            held.matches_browser(&call.request.conversation, u64::from(call.request.reply))
        })
    });
    if !owns_slot || active.is_some() || rejected.is_some() {
        call.cancel.store(true, Ordering::Release);
        service.request_stop();
    }
    let admission = TextAdmission::from_native(
        call.cancel.clone(),
        service.account_revision.clone(),
        call.account_revision,
    );
    let (proof, commit) = browser_reservation::committed(start, call.request.reply, admission);
    match connection.start_browser(scope, proof, commit, &service.stop) {
        Ok(()) => {
            *active = Some(Active {
                browser: true,
                delivery: None,
                call,
                items: HashMap::new(),
                cancel_sent: false,
            });
            Ok(())
        }
        Err(failure) => {
            let (fault, unadopted) = failure.into_parts();
            if let Some(unadopted) = unadopted {
                // Only the impossible preexisting-owner branch returns this
                // exact no-provider local obligation. Keep its waiter too.
                let value = Rejected {
                    owner: unadopted,
                    call,
                    fault: page_fault(fault),
                };
                if let Some(previous) = rejected.replace(value) {
                    park(service, None, Some(previous));
                }
            } else if active.is_none() {
                *active = Some(Active {
                    browser: true,
                    delivery: Some(ReplyDelivery::Failed(page_fault(fault))),
                    call,
                    items: HashMap::new(),
                    cancel_sent: true,
                });
            } else {
                // Native's retained failure cannot alias another active turn.
                // No release or synthetic provider outcome follows this fault.
                park(service, None, None);
                let _ = call
                    .result
                    .send(ReplyDelivery::Failed(PageAssistNativeFaultV1::Protocol));
            }
            service.request_stop();
            Err(fault)
        }
    }
}

fn release(service: &Service, key: &str, reply: u64) -> bool {
    let mut held = service
        .settling
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if held
        .as_ref()
        .is_some_and(|value| value.matches_browser(key, reply))
    {
        if let Some(held) = held.take() {
            held.release_browser();
            return true;
        }
    }
    false
}

pub(super) fn settled(
    service: &Service,
    key: &str,
    reply: u64,
    local: zephium_codex_broker::connection::BrowserLocalOutcome,
    active: &mut Option<Active>,
) {
    if !release(service, key, reply) {
        return;
    }
    let matches = active.as_ref().is_some_and(|current| {
        current.browser
            && current.call.request.conversation == key
            && u64::from(current.call.request.reply) == reply
    });
    if matches {
        if let Some(current) = active.take() {
            finish_browser(
                current,
                NativeBrowserSettlement::Joined {
                    local: match local {
                        zephium_codex_broker::connection::BrowserLocalOutcome::Observed => {
                            NativeBrowserLocalOutcome::Observed
                        }
                        zephium_codex_broker::connection::BrowserLocalOutcome::ResultUnobserved => {
                            NativeBrowserLocalOutcome::ResultUnobserved
                        }
                    },
                },
            );
        }
    }
}

pub(super) fn historical(
    service: &Service,
    scope: &conversation::Scope,
    active: &mut Option<Active>,
    events: Vec<ScopedEvent>,
) {
    for event in events {
        if event.scope() != scope {
            continue;
        }
        let key = event.conversation().map(|value| value.as_str().to_owned());
        let matches = active
            .as_ref()
            .is_some_and(|value| key.as_deref() == Some(value.call.request.conversation.as_str()));
        let outcome = match event.into_event() {
            Event::BrowserSettled { reply, local } => {
                if let Some(key) = key.as_deref() {
                    settled(service, key, reply, local, active);
                }
                None
            }
            Event::Conversation(conversation::Event::Terminal(terminal)) if matches => {
                Some(NativeConversationOutcome {
                    status: match terminal.outcome {
                        conversation::Outcome::Completed => NativeConversationStatus::Completed,
                        conversation::Outcome::Interrupted => NativeConversationStatus::Interrupted,
                        conversation::Outcome::Failed => NativeConversationStatus::Failed,
                        conversation::Outcome::Unknown => NativeConversationStatus::Unknown,
                    },
                    final_response: terminal.final_response().map(str::to_owned),
                })
            }
            Event::HistoryUncertain(terminal) if matches => Some(NativeConversationOutcome {
                status: NativeConversationStatus::HistoryUncertain,
                final_response: terminal.final_response().map(str::to_owned),
            }),
            Event::NeverStarted { reply }
                if matches
                    && active
                        .as_ref()
                        .is_some_and(|value| u64::from(value.call.request.reply) == reply) =>
            {
                Some(NativeConversationOutcome {
                    status: NativeConversationStatus::NeverStarted,
                    final_response: None,
                })
            }
            _ => None,
        };
        if let Some(outcome) = outcome {
            deliver_outcome(active, outcome);
        }
    }
}

fn finish_browser(current: Active, settlement: NativeBrowserSettlement) {
    match current.delivery {
        Some(ReplyDelivery::Settled(outcome)) => {
            current.call.result.browser(NativeBrowserCompletion {
                outcome,
                settlement,
            })
        }
        Some(ReplyDelivery::Failed(fault)) => {
            let _ = current.call.result.send(ReplyDelivery::Failed(fault));
        }
        None => {
            let _ = current.call.result.send(ReplyDelivery::Failed(
                PageAssistNativeFaultV1::OutcomeUnknown,
            ));
        }
    }
}

pub(super) fn quarantine_waiter(active: &mut Option<Active>) {
    if let Some(current) = active.take() {
        finish_browser(current, NativeBrowserSettlement::Quarantined);
    }
}

/// Ordinary admission is already retired. Keep the same Receiver alive until a
/// pre-say reservation drops or synchronously hands off, then preserve the port
/// through actual original+local settlement. Polling here performs no RPC/tool.
pub(super) struct Exit {
    pub(super) rejected: Option<Rejected>,
    pub(super) pending: Option<Command>,
}

pub(super) fn drain(
    service: &Arc<Service>,
    registry: &std::sync::Weak<Registry>,
    receiver: &admission::Receiver<Command>,
    connection: Connection,
    scope: &conversation::Scope,
    active: &mut Option<Active>,
    exit: Exit,
) -> Option<Connection> {
    let Exit {
        mut rejected,
        mut pending,
    } = exit;
    service.request_stop(); // closes readiness under the same reservation gate
    let mut connection = Some(connection);
    let mut needs_retire = true;
    loop {
        let command = pending.take().or_else(|| receiver.try_recv().ok());
        if let Some(command) = command {
            match command {
                Command::PrepareBrowser(call) => {
                    let _ = call.result.send(Err(Fault::ScopeChanged));
                }
                Command::Select(call) => {
                    let _ = call.result.send(Err(CodexFaultV1::ScopeChanged));
                }
                Command::Reply(mut call) => {
                    if call.turn.is_some() {
                        let _ = adopt_reply_turn(service, &mut call);
                    }
                    if let Some(start) = call.browser.take() {
                        if let Some(connection) = connection.as_mut() {
                            let _ = self::start(
                                service,
                                connection,
                                scope,
                                call,
                                *start,
                                active,
                                &mut rejected,
                            );
                            needs_retire = true;
                        }
                    } else {
                        let _ = call.result.send(ReplyDelivery::Failed(
                            PageAssistNativeFaultV1::OutcomeUnknown,
                        ));
                    }
                }
                _ => {}
            }
        }
        if let Some(owner) = connection.as_mut() {
            if needs_retire && owner.browser_settlement_pending() {
                let reason = if registry.upgrade().is_some_and(|registry| {
                    registry.scope(&service.profile.to_string()) == Some(service.epoch)
                }) {
                    BrowserRevokeReason::ConnectionLost
                } else {
                    BrowserRevokeReason::ScopeChanged
                };
                let _ = owner.retire_browser(scope, reason);
                needs_retire = false;
            }
            if let Ok(events) = owner.take_failure_events(scope) {
                historical(service, scope, active, events);
            }
            match owner.poll_browser_settlement(scope) {
                Ok(events) => historical(service, scope, active, events),
                Err(fault) => set_fault(service, super::fault(fault)),
            }
        }
        if let Some(local) = rejected.as_mut() {
            if let Ok(Some(receipt)) = local.owner.poll_settlement() {
                if receipt.scope() == scope {
                    if release(service, receipt.conversation().as_str(), receipt.reply()) {
                        if let Some(local) = rejected.take() {
                            let _ = local.call.result.send(ReplyDelivery::Failed(local.fault));
                        }
                    }
                }
            }
        }
        let original_lost = connection
            .as_ref()
            .is_some_and(|owner| owner.browser_local_receipt_lost());
        let rejected_lost = rejected
            .as_ref()
            .is_some_and(|value| value.owner.local_receipt_lost());
        if original_lost || rejected_lost {
            // Extraction independently requires factual physical-original
            // settlement plus irrecoverable aggregate receipt loss. A generic
            // fault or missing individual tool output never authorizes this.
            let original = if original_lost {
                let owner = connection.take()?;
                match owner.quarantine_browser() {
                    Ok(custody) => Some(custody),
                    Err(owner) => {
                        connection = Some(*owner);
                        None
                    }
                }
            } else {
                None
            };
            if original.is_some()
                || (rejected_lost
                    && connection
                        .as_ref()
                        .is_none_or(|owner| !owner.browser_settlement_pending()))
            {
                if rejected
                    .as_ref()
                    .is_some_and(|value| !value.owner.local_receipt_lost())
                {
                    // A still-live private receiver remains a tracked obligation.
                    park(service, original, None);
                    set_fault(service, CodexFaultV1::RecoveryRequired);
                } else {
                    park(service, original, rejected.take());
                    quarantine_waiter(active);
                    set_fault(service, CodexFaultV1::RecoveryRequired);
                    return None;
                }
            }
        }
        if rejected.is_none()
            && connection
                .as_ref()
                .is_none_or(|owner| !owner.browser_settlement_pending())
            && service.queue.browser_handoff_lost()
        {
            // No port remains to observe: a committed queue envelope/owner
            // was lost. Preserve sticky local quarantine, physically close the
            // original owner on return, and do not keep a futile polling loop.
            park(service, None, None);
            quarantine_waiter(active);
            set_fault(service, CodexFaultV1::RecoveryRequired);
            return None;
        }
        if !service.queue.browser_occupied()
            && rejected.is_none()
            && connection
                .as_ref()
                .is_none_or(|owner| !owner.browser_settlement_pending())
        {
            return connection;
        }
        // An actual effect/receiver or outstanding pre-say reservation remains.
        // There is no deadline-based release, resend, or human-wait polling work.
        std::thread::sleep(Duration::from_millis(10));
    }
}
