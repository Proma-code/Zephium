//! Page-owned BrowserV1 dispatcher. The UI owns a waiter, not the physical tool
//! future. Native admission stays retained until this dispatcher returns its
//! private local-settlement receipt as well as the original runtime settling.
use super::super::native_browser::{
    self, NativeBrowserInput, NativeBrowserLocalSettlement, NativeBrowserToolOutput,
    PageBrowserBridge,
};
use super::*;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::watch;

pub(crate) struct BrowserOwner {
    identity: Arc<()>,
    profile: ProfileId,
    _cancel: Arc<Cancel>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl BrowserOwner {
    pub(crate) fn blocks(&self, profile: ProfileId) -> bool {
        self.profile == profile
    }
}

struct Waiter {
    done: oneshot::Receiver<()>,
    cancel: Arc<Cancel>,
    returned: bool,
}
impl Future for Waiter {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match Pin::new(&mut self.done).poll(cx) {
            Poll::Ready(_) => {
                self.returned = true;
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
impl Drop for Waiter {
    fn drop(&mut self) {
        if !self.returned {
            self.cancel.cancel(CancelReason::Gone);
        }
    }
}

/// The service registry retains the JoinHandle; a closed UI only cancels its
/// exact owner. A panic intentionally leaves the registry entry/quarantine and
/// drops the unsatisfied local-settlement sender, never forging completion.
pub(super) fn task(reply: Reply) -> super::super::TurnTask {
    let key = (reply.conversation.clone(), reply.reply);
    let identity = Arc::new(());
    let service = reply.service.clone();
    let cancel = reply.cancel.clone();
    let (start, started) = oneshot::channel();
    let (done, finished) = oneshot::channel();
    service.state().native_owners.insert(
        key.clone(),
        BrowserOwner {
            identity: identity.clone(),
            profile: reply.profile,
            _cancel: cancel.clone(),
            join: None,
        },
    );
    let owner_service = service.clone();
    let owner_key = key.clone();
    let owner_identity = identity.clone();
    let join = tokio::spawn(async move {
        if started.await.is_err() {
            return;
        }
        run(reply).await;
        let mut state = owner_service.state();
        if state
            .native_owners
            .get(&owner_key)
            .is_some_and(|owner| Arc::ptr_eq(&owner.identity, &owner_identity))
        {
            state.native_owners.remove(&owner_key);
        }
        drop(state);
        let _ = done.send(());
    });
    if let Some(owner) = service.state().native_owners.get_mut(&key) {
        owner.join = Some(join);
    }
    let _ = start.send(());
    Box::pin(Waiter {
        done: finished,
        cancel,
        returned: false,
    })
}

pub(super) struct Turn {
    bridge: PageBrowserBridge,
    snapshots: watch::Receiver<Option<String>>,
}

fn latch(callbacks: &mut NativeCallbackState, fault: PageAssistNativeFaultV1) {
    if callbacks.fault.is_none()
        || (callbacks.fault == Some(PageAssistNativeFaultV1::Cancelled)
            && fault != PageAssistNativeFaultV1::Cancelled)
    {
        callbacks.fault = Some(fault);
    }
}

pub(crate) fn commit_native_browser(reply: &Reply, native: Box<dyn NativeConversationReservation>) {
    let budget = reply
        .native_budget
        .as_ref()
        .expect("BrowserV1 owns one budget");
    let (snapshots, incoming) = watch::channel(None);
    let active = Arc::new(std::sync::Mutex::new(NativeCallbackState {
        open: true,
        fault: None,
    }));
    let revoked_callbacks = active.clone();
    let revoked_cancel = reply.cancel.clone();
    let revoke = native_browser::NativeBrowserRevocation::new(
        reply
            .native_admission
            .as_ref()
            .expect("BrowserV1 admission")
            .clone(),
        move |reason| {
            use native_browser::NativeBrowserRevokeReason;
            let fault = match reason {
                NativeBrowserRevokeReason::ScopeChanged => PageAssistNativeFaultV1::ScopeChanged,
                NativeBrowserRevokeReason::ConnectionLost => {
                    PageAssistNativeFaultV1::OutcomeUnknown
                }
                NativeBrowserRevokeReason::Protocol => PageAssistNativeFaultV1::Protocol,
                NativeBrowserRevokeReason::Cancelled => PageAssistNativeFaultV1::Cancelled,
            };
            let mut callbacks = revoked_callbacks.lock().unwrap_or_else(|p| p.into_inner());
            latch(&mut callbacks, fault);
            callbacks.open = false;
            drop(callbacks);
            revoked_cancel.cancel(CancelReason::Gone);
        },
    );
    let (bridge, native_bridge) = native_browser::bridge(budget.view(), revoke);
    let callback_state = active.clone();
    let callback_admission = reply
        .native_admission
        .as_ref()
        .expect("BrowserV1 admission")
        .clone();
    let cancel = reply.cancel.clone();
    let events: super::super::port::NativeConversationEvents = Arc::new(move |event| {
        let mut state = callback_state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(fault) = state.fault {
            return Err(fault);
        }
        if !state.open || cancel.reason().is_some() || callback_admission.is_closed() {
            return Err(PageAssistNativeFaultV1::Cancelled);
        }
        let NativeConversationEvent::TextSnapshot { text } = event;
        if text.len() > zephium_page_assist::conversation::MAX_TEXT_BYTES {
            latch(&mut state, PageAssistNativeFaultV1::Limit);
            cancel.cancel(CancelReason::Stopped);
            return Err(PageAssistNativeFaultV1::Limit);
        }
        snapshots.send_replace(Some(text));
        Ok(())
    });
    let future = native.commit_browser(
        reply.reply,
        reply.cancel.native.clone(),
        events,
        native_bridge,
    );
    *reply.native.lock().unwrap_or_else(|p| p.into_inner()) = Some(NativeTurn {
        future: NativeTurnFuture::Browser(future),
        callback_state: active.clone(),
        _lease: NativeCallbackLease(active),
        deadline: tokio::time::Instant::now() + CALL_TIMEOUT,
        browser: Some(Turn {
            bridge,
            snapshots: incoming,
        }),
    });
}

fn budget_fault(fault: super::super::native_budget::NativeBudgetFault) -> PageAssistNativeFaultV1 {
    use super::super::native_budget::NativeBudgetFault;
    match fault {
        NativeBudgetFault::Expired => PageAssistNativeFaultV1::Timeout,
        NativeBudgetFault::Closed => PageAssistNativeFaultV1::Cancelled,
        NativeBudgetFault::InvalidTransition | NativeBudgetFault::ClockRegressed => {
            PageAssistNativeFaultV1::Protocol
        }
    }
}

fn fence(
    reply: &Reply,
    callbacks: &Arc<std::sync::Mutex<NativeCallbackState>>,
    fault: PageAssistNativeFaultV1,
) {
    {
        let mut callbacks = callbacks.lock().unwrap_or_else(|p| p.into_inner());
        callbacks.open = false;
        latch(&mut callbacks, fault);
    }
    if let Some(budget) = &reply.native_budget {
        budget.seal();
    }
    reply.cancel.cancel(CancelReason::Stopped);
    if fault == PageAssistNativeFaultV1::Cancelled {
        reply.apply(PageAssistEventV1::Cancelled);
    } else {
        reply.apply(PageAssistEventV1::Failed {
            fault: PageAssistFaultV1::NativeConversation { fault },
        });
    }
}

fn final_fault(reply: &Reply, fault: PageAssistNativeFaultV1) -> End {
    if fault == PageAssistNativeFaultV1::Cancelled {
        End::Cancelled(reply.cancel.reason().unwrap_or(CancelReason::Stopped))
    } else {
        native_fault(fault)
    }
}

/// Rewrites the complete original snapshot against this reply's actual read
/// ledger. It is a replacement, never guessed append text; cards keep their own
/// blocks and cannot be overwritten by a late model snapshot.
fn rewrite_snapshot(
    data: &mut super::super::ReplyData,
    source: &str,
) -> Result<(String, Vec<PageAssistCiteV1>), PageAssistNativeFaultV1> {
    // Enforce the source bound before rewriting; stripping invalid references
    // must not launder an oversized original completed-item selection.
    if source.len() > zephium_page_assist::conversation::MAX_TEXT_BYTES {
        return Err(PageAssistNativeFaultV1::Limit);
    }
    let mut rewriter = CiteRewriter::default();
    let mut cites = Vec::new();
    let mut text = rewriter.push(source, &mut |reference| {
        let read = data.reads.get(usize::from(reference.read))?;
        let cite = data
            .book
            .cite(reference, &read.page, &read.tab, read.epoch)?;
        if !cites
            .iter()
            .any(|known: &PageAssistCiteV1| known.n == cite.n)
        {
            cites.push(cite.clone());
        }
        Some(cite.n)
    });
    text.push_str(&rewriter.finish());
    if text.len() > zephium_page_assist::conversation::MAX_TEXT_BYTES {
        return Err(PageAssistNativeFaultV1::Limit);
    }
    Ok((text, cites))
}

fn show_snapshot(reply: &Reply, block: u16, source: &str) -> Result<(), PageAssistNativeFaultV1> {
    let (text, cites) = reply
        .with_data(|data| rewrite_snapshot(data, source))
        .ok_or(PageAssistNativeFaultV1::ScopeChanged)??;
    if !reply.apply(PageAssistEventV1::TextSnapshot { block, text })
        || !reply.apply(PageAssistEventV1::Cites { block, cites })
    {
        return Err(PageAssistNativeFaultV1::ScopeChanged);
    }
    Ok(())
}

/// The exclusive successful native future is the sealed historical result. It
/// cannot rearm callbacks, the budget, or effects. All scope checks and the
/// entire final projection occur under one Page lock, including A→B→A lifetime.
fn project_terminal(
    reply: &Reply,
    block: u16,
    outcome: super::super::port::NativeConversationOutcome,
    local_fault: Option<PageAssistNativeFaultV1>,
) -> Result<(), PageAssistNativeFaultV1> {
    use zephium_page_assist::conversation::NativeTerminalProjection;
    if outcome.status == NativeConversationStatus::NeverStarted && outcome.final_response.is_some()
    {
        return Err(PageAssistNativeFaultV1::Protocol);
    }
    let event = {
        let mut state = reply.service.state();
        if state.current.get(&reply.space) != Some(&reply.conversation)
            || reply
                .current_epoch
                .as_ref()
                .zip(state.current_epochs.get(&reply.space))
                .is_none_or(|(captured, current)| !Arc::ptr_eq(captured, current))
            || state
                .running
                .get(&reply.conversation)
                .is_none_or(|running| {
                    running.profile != reply.profile
                        || running.reply != reply.reply
                        || !Arc::ptr_eq(&running.cancel, &reply.cancel)
                })
        {
            return Err(PageAssistNativeFaultV1::ScopeChanged);
        }
        let live = state
            .live
            .get_mut(&reply.conversation)
            .filter(|live| live.profile == reply.profile)
            .ok_or(PageAssistNativeFaultV1::ScopeChanged)?;
        let data = live
            .replies
            .get_mut(&reply.reply)
            .filter(|data| data.native_outcome.is_none())
            .ok_or(PageAssistNativeFaultV1::ScopeChanged)?;
        let (text, cites) =
            rewrite_snapshot(data, outcome.final_response.as_deref().unwrap_or_default())?;
        let event = live
            .conversation
            .project_native_terminal(
                reply.reply,
                NativeTerminalProjection {
                    block,
                    status: outcome.status,
                    final_text: outcome.final_response.as_ref().map(|_| text),
                    cites,
                    local_fault,
                },
            )
            .ok_or(PageAssistNativeFaultV1::Protocol)?;
        data.native_outcome = Some(outcome);
        live.revision = Arc::new(());
        event
    };
    reply.service.emit_all([event]);
    Ok(())
}

async fn timer(
    deadline: Result<std::time::Instant, super::super::native_budget::NativeBudgetFault>,
) {
    if let Ok(deadline) = deadline {
        tokio::time::sleep_until(deadline.into()).await;
    }
}

pub(super) async fn body(reply: &Reply, native: NativeTurn) -> End {
    let NativeTurn {
        future,
        callback_state,
        _lease,
        browser,
        ..
    } = native;
    let NativeTurnFuture::Browser(mut future) = future else {
        return native_fault(PageAssistNativeFaultV1::Protocol);
    };
    let Some(Turn {
        bridge,
        mut snapshots,
    }) = browser
    else {
        return native_fault(PageAssistNativeFaultV1::Protocol);
    };
    let PageBrowserBridge {
        prepared,
        mut requests,
        settled,
    } = bridge;
    let mut timing = reply
        .native_budget
        .as_ref()
        .expect("BrowserV1 budget")
        .view();
    let mut fault = None;
    let mut native_result = None;
    let mut admission_closed = false;
    let mut tools_text = Text::default();
    let text_block = reply.next_block();
    if !reply.apply(PageAssistEventV1::TextSnapshot {
        block: text_block,
        text: String::new(),
    }) {
        fault = Some(PageAssistNativeFaultV1::ScopeChanged);
    }
    // No provider input exists before this native-owned current context returns.
    // Do not replay prior Page messages or rewrite the original person's words.
    let prepare = reply.context();
    tokio::pin!(prepare);
    let input = loop {
        if fault.is_none() {
            if reply.cancel.reason().is_some() {
                fault = Some(PageAssistNativeFaultV1::Cancelled);
            } else if let Err(error) = timing.deadline() {
                fault = Some(budget_fault(error));
            }
            if let Some(error) = fault {
                fence(reply, &callback_state, error);
            }
        }
        let deadline = timing.deadline();
        tokio::select! {
            biased;
            result = &mut future, if native_result.is_none() => {
                native_result = Some(result);
                fault.get_or_insert(PageAssistNativeFaultV1::Cancelled);
                fence(reply, &callback_state, fault.unwrap());
            }
            _ = reply.browser_closed_wait(), if !admission_closed => {
                admission_closed = true;
                callback_state.lock().unwrap_or_else(|p| p.into_inner()).open = false;
            }
            _ = reply.cancel.cancelled(), if fault.is_none() => {
                fault = Some(PageAssistNativeFaultV1::Cancelled);
                fence(reply, &callback_state, fault.unwrap());
            }
            _ = timing.changed(), if fault.is_none() => {}
            _ = timer(deadline), if fault.is_none() => {}
            parts = &mut prepare => break parts.join("\n\n"),
        }
    };
    let input = if let Some(error) = fault {
        Err(error)
    } else if reply.browser_closed() {
        // Only the original owner can classify this as NeverStarted. The local
        // seal merely refuses an input that has not been handed off yet.
        admission_closed = true;
        Err(PageAssistNativeFaultV1::Cancelled)
    } else {
        NativeBrowserInput::new(input)
    };
    if let Err(error) = &input {
        if !admission_closed || fault.is_some() {
            fault.get_or_insert(*error);
            fence(reply, &callback_state, *error);
        }
    }
    if prepared.send(input).is_err() && fault.is_none() {
        fault = Some(PageAssistNativeFaultV1::Protocol);
        fence(reply, &callback_state, fault.unwrap());
    }
    let mut expected_ordinal = 1u32;
    let mut snapshots_open = true;
    let mut handed_off = false;
    loop {
        admission_closed |= reply.browser_closed();
        if fault.is_some() || native_result.is_some() || admission_closed {
            requests.close();
            while let Some(request) = requests.recv().await {
                // A queued unresolved browser request was interrupted; unlike
                // an idle seal, this is a local result that was not completed.
                fault.get_or_insert(PageAssistNativeFaultV1::LocalInterruption);
                let _ = request
                    .response
                    .send(Err(fault.unwrap_or(PageAssistNativeFaultV1::Cancelled)));
            }
            break;
        }
        if reply.cancel.reason().is_some() {
            fault = Some(PageAssistNativeFaultV1::Cancelled);
        } else if let Err(error) = timing.deadline() {
            fault = Some(budget_fault(error));
        }
        if let Some(error) = fault {
            fence(reply, &callback_state, error);
            continue;
        }
        let deadline = timing.deadline();
        tokio::select! {
            biased;
            result = &mut future => { native_result = Some(result); requests.close(); }
            _ = reply.browser_closed_wait() => {
                admission_closed = true;
                callback_state.lock().unwrap_or_else(|p| p.into_inner()).open = false;
            }
            _ = reply.cancel.cancelled() => {
                fault = Some(PageAssistNativeFaultV1::Cancelled);
                fence(reply, &callback_state, fault.unwrap());
            }
            _ = timing.changed() => {}
            _ = timer(deadline) => {}
            changed = snapshots.changed(), if snapshots_open => {
                if changed.is_err() { snapshots_open = false; }
                if changed.is_ok() {
                    let text = snapshots.borrow_and_update().clone();
                    if let Some(text) = text {
                        if let Err(error) = show_snapshot(reply, text_block, &text) {
                            fault = Some(error); fence(reply, &callback_state, error);
                        }
                    }
                }
            }
            request = requests.recv() => {
                let Some(request) = request else { break; };
                if reply.browser_closed() {
                    admission_closed = true;
                    fault.get_or_insert(PageAssistNativeFaultV1::LocalInterruption);
                    let _ = request.response.send(Err(PageAssistNativeFaultV1::Cancelled));
                    continue;
                }
                if request.ordinal != expected_ordinal {
                    let _ = request.response.send(Err(PageAssistNativeFaultV1::Protocol));
                    fault = Some(PageAssistNativeFaultV1::Protocol);
                    fence(reply, &callback_state, fault.unwrap()); continue;
                }
                expected_ordinal += 1;
                if handed_off {
                    let _ = request.response.send(NativeBrowserToolOutput::new(Err("Work was offered to the person; no further Page tools may run in this reply.".into())));
                    continue;
                }
                let Some(call) = tools::parse(&request.name, &request.arguments) else {
                    let output = NativeBrowserToolOutput::new(Err("The tool arguments do not match the fixed Page contract.".into()));
                    let _ = request.response.send(output);
                    continue;
                };
                if let Err(error) = reply.browser_admission() {
                    let _ = request.response.send(Err(error)); fault = Some(error);
                    fence(reply, &callback_state, error); continue;
                }
                handed_off = matches!(&call, Call::HandOff { .. });
                let tool = run_tool(reply, &mut tools_text, call);
                tokio::pin!(tool);
                let outcome = loop {
                    let deadline = timing.deadline();
        tokio::select! {
                        biased;
                        result = &mut future, if native_result.is_none() => {
                            native_result = Some(result);
                            fault.get_or_insert(PageAssistNativeFaultV1::Cancelled);
                            fence(reply, &callback_state, fault.unwrap());
                        }
                        _ = reply.browser_closed_wait(), if !admission_closed => {
                            admission_closed = true;
                            fault.get_or_insert(PageAssistNativeFaultV1::LocalInterruption);
                            callback_state.lock().unwrap_or_else(|p| p.into_inner()).open = false;
                        }
                        _ = reply.cancel.cancelled(), if fault.is_none() => {
                            fault = Some(PageAssistNativeFaultV1::Cancelled);
                            fence(reply, &callback_state, fault.unwrap());
                        }
                        _ = timing.changed(), if fault.is_none() => {}
                        _ = timer(deadline), if fault.is_none() => {
                            if let Err(error) = timing.deadline() {
                                fault = Some(budget_fault(error)); fence(reply, &callback_state, fault.unwrap());
                            }
                        }
                        result = &mut tool => break result,
                    }
                };
                if reply.browser_closed() {
                    admission_closed = true;
                    fault.get_or_insert(PageAssistNativeFaultV1::LocalInterruption);
                }
                if fault.is_none() {
                    if let Err(error) = reply.browser_admission() { fault = Some(error); fence(reply, &callback_state, error); }
                }
                let output = if let Some(error) = fault { Err(error) } else {
                    match outcome {
                        Ok(result) => NativeBrowserToolOutput::new(result),
                        Err(End::Cancelled(_)) => Err(PageAssistNativeFaultV1::Cancelled),
                        Err(_) => Err(PageAssistNativeFaultV1::Protocol),
                    }
                };
                if let Err(error) = &output {
                    fault = Some(*error);
                    if *error != PageAssistNativeFaultV1::LocalInterruption {
                        fence(reply, &callback_state, *error);
                    }
                }
                if request.response.send(output).is_err() && fault.is_none() {
                    // Local effect returned, but original delivery is uncertain.
                    fault = Some(PageAssistNativeFaultV1::OutcomeUnknown);
                    fence(reply, &callback_state, fault.unwrap());
                }
            }
        }
    }
    let callback_fault = {
        let mut callbacks = callback_state.lock().unwrap_or_else(|p| p.into_inner());
        callbacks.open = false;
        callbacks.fault
    };
    if let Some(error) = callback_fault {
        fault = Some(error);
    }
    // Only here can the original owner release its second custody obligation.
    // Channel close/Stop never bypasses an entered local future above.
    let _ = settled.send(NativeBrowserLocalSettlement::returned());
    let result = if let Some(result) = native_result {
        result
    } else {
        // Admission/UI deadlines remain live during physical native drain. An
        // expired/cancelled waiter never drops the original owner's future.
        loop {
            if fault.is_none() {
                if reply.cancel.reason().is_some() {
                    fault = Some(PageAssistNativeFaultV1::Cancelled);
                } else if let Err(error) = timing.deadline() {
                    fault = Some(budget_fault(error));
                }
                if let Some(error) = fault {
                    fence(reply, &callback_state, error);
                }
            }
            let deadline = timing.deadline();
            tokio::select! {
                biased;
                result = &mut future => break result,
                _ = reply.cancel.cancelled(), if fault.is_none() => {
                    fault = Some(PageAssistNativeFaultV1::Cancelled);
                    fence(reply, &callback_state, fault.unwrap());
                }
                _ = timing.changed(), if fault.is_none() => {}
                _ = timer(deadline), if fault.is_none() => {}
            }
        }
    };
    // Native authority may be revoked while the original owner drains after
    // local settlement. Preserve that substantive fault, not its cancel wake.
    if let Some(error) = callback_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .fault
    {
        fault = Some(error);
    }
    let completion = match result {
        Ok(completion) => completion,
        Err(error) => {
            return final_fault(
                reply,
                fault
                    .filter(|fault| {
                        !matches!(
                            fault,
                            PageAssistNativeFaultV1::Cancelled
                                | PageAssistNativeFaultV1::LocalInterruption
                        )
                    })
                    .unwrap_or(error),
            )
        }
    };
    let native_browser::NativeBrowserCompletion {
        outcome,
        settlement,
    } = completion;
    if !matches!(
        settlement,
        native_browser::NativeBrowserSettlement::Joined {
            local: native_browser::NativeBrowserLocalOutcome::Observed,
        }
    ) {
        // Physical quarantine and unobserved delivery are not ordinary success,
        // even when the provider's original answer is already authoritative.
        fault.get_or_insert(PageAssistNativeFaultV1::LocalInterruption);
    }
    // Do not let a revocation wake erase a sealed historical terminal. Local
    // output/structural faults still remain substantive and final text is bounded
    // again before any projection. Provider Unknown is never promoted to Done.
    let local_fault = if outcome.status == NativeConversationStatus::Completed {
        fault.map(|fault| match fault {
            // These are admission/owner interruptions, not evidence that a
            // provider outcome already received has become unknown again.
            PageAssistNativeFaultV1::ScopeChanged
            | PageAssistNativeFaultV1::OutcomeUnknown
            | PageAssistNativeFaultV1::Cancelled
            | PageAssistNativeFaultV1::Timeout => PageAssistNativeFaultV1::LocalInterruption,
            substantive => substantive,
        })
    } else {
        // This copy explicitly says the provider finished the reply. It is not
        // a claim we can make for Interrupted/Failed/Unknown or no outcome.
        fault.filter(|fault| *fault != PageAssistNativeFaultV1::LocalInterruption)
    };
    match project_terminal(reply, text_block, outcome, local_fault) {
        Ok(()) => End::NativeProjected,
        Err(error) => native_fault(error),
    }
}

#[cfg(test)]
mod fault_tests {
    use super::*;
    #[test]
    fn substantive_fault_survives_a_later_cancellation_wake() {
        let mut state = NativeCallbackState {
            open: true,
            fault: None,
        };
        latch(&mut state, PageAssistNativeFaultV1::Timeout);
        latch(&mut state, PageAssistNativeFaultV1::Cancelled);
        assert_eq!(state.fault, Some(PageAssistNativeFaultV1::Timeout));
        let mut state = NativeCallbackState {
            open: true,
            fault: None,
        };
        latch(&mut state, PageAssistNativeFaultV1::Cancelled);
        latch(&mut state, PageAssistNativeFaultV1::ScopeChanged);
        latch(&mut state, PageAssistNativeFaultV1::Protocol);
        assert_eq!(state.fault, Some(PageAssistNativeFaultV1::ScopeChanged));
    }
}
