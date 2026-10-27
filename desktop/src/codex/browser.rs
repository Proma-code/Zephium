//! Nonblocking adapter over the existing Page browser owner. This module is
//! used only by the closed BrowserV1 qualification path until the native, Store
//! and Page cohort is qualified. It neither runs tools nor grants authority.
use std::{sync::Arc, time::Instant};
use tokio::sync::{mpsc, oneshot};
use zephium_codex_broker::browser_port::{
    BrowserBudget, BrowserCompletionKey, BrowserDispatch, BrowserDispatchOutcome, BrowserInput,
    BrowserInputPoll, BrowserLocalSettlement, BrowserManifest, BrowserPort, BrowserPortFault,
    BrowserResultKey, BrowserResultPoll, BrowserRevokeReason, BrowserToolDefinition,
};
use zephium_ipc::page_assist::PageAssistNativeFaultV1 as PageFault;
use zephium_work_composition::page_assist::port::{
    NativeBrowserBridge, NativeBrowserInput, NativeBrowserLocalSettlement, NativeBrowserManifest,
    NativeBrowserRevocation, NativeBrowserRevokeReason, NativeBrowserToolOutput,
    NativeBrowserToolRequest, NativeBudgetView, NATIVE_BROWSER_TOOL_CALLS,
};

/// Only the Page-owned fixed definitions enter the native policy constructor.
/// No caller-supplied digest, runtime version, user text or readout is accepted.
pub(super) fn manifest(value: NativeBrowserManifest) -> Result<BrowserManifest, BrowserPortFault> {
    if value.version() != "zephium-page-browser-v1" {
        return Err(BrowserPortFault::Protocol);
    }
    BrowserManifest::from_native_page_v1(
        value.instructions().to_owned(),
        value
            .tools()
            .iter()
            .map(|tool| BrowserToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                schema: tool.schema.clone(),
            })
            .collect(),
    )
}

struct PendingResult {
    key: Option<BrowserResultKey>,
    receiver: oneshot::Receiver<Result<NativeBrowserToolOutput, PageFault>>,
    lost: bool,
}

/// Every original writer gets the same allocation wrapping Page's original
/// live budget. This adapter cannot renew its clock or human-wait authority.
struct PageBudget(NativeBudgetView);
impl BrowserBudget for PageBudget {
    fn deadline(&self) -> Result<Instant, BrowserPortFault> {
        self.0.deadline().map_err(|_| BrowserPortFault::Budget)
    }
    fn hard_deadline(&self) -> Result<Instant, BrowserPortFault> {
        self.0.hard_deadline().map_err(|_| BrowserPortFault::Budget)
    }
}

pub(super) struct PageBrowserPort {
    input: Option<oneshot::Receiver<Result<NativeBrowserInput, PageFault>>>,
    requests: Option<mpsc::Sender<NativeBrowserToolRequest>>,
    timing: Arc<dyn BrowserBudget>,
    settlement: oneshot::Receiver<NativeBrowserLocalSettlement>,
    revoke: NativeBrowserRevocation,
    pending: Vec<PendingResult>,
    accepted: u32,
    completion: Option<BrowserCompletionKey>,
    revoked: Option<BrowserRevokeReason>,
    settlement_lost: bool,
    settled: bool,
}

impl PageBrowserPort {
    pub(super) fn new(bridge: NativeBrowserBridge) -> Self {
        let (input, requests, timing, settlement, revoke) = bridge.into_parts();
        Self {
            input: Some(input),
            requests: Some(requests),
            timing: Arc::new(PageBudget(timing)),
            settlement,
            revoke,
            pending: Vec::new(),
            accepted: 0,
            completion: None,
            revoked: None,
            settlement_lost: false,
            settled: false,
        }
    }
}

impl BrowserPort for PageBrowserPort {
    fn poll_input(&mut self) -> BrowserInputPoll {
        if let Some(reason) = self.revoked {
            self.input.take();
            return BrowserInputPoll::Failed(match reason {
                BrowserRevokeReason::Cancelled => BrowserPortFault::Cancelled,
                BrowserRevokeReason::ScopeChanged => BrowserPortFault::ScopeChanged,
                BrowserRevokeReason::ConnectionLost => BrowserPortFault::ConnectionLost,
                BrowserRevokeReason::Protocol => BrowserPortFault::Protocol,
            });
        }
        let Some(input) = self.input.as_mut() else {
            return BrowserInputPoll::Failed(BrowserPortFault::Protocol);
        };
        match input.try_recv() {
            Ok(Ok(value)) => {
                self.input.take();
                match BrowserInput::from_native_page(value.into_text()) {
                    Ok(value) => BrowserInputPoll::Ready(value),
                    Err(fault) => BrowserInputPoll::Failed(fault),
                }
            }
            Ok(Err(fault)) => {
                self.input.take();
                BrowserInputPoll::Failed(page_fault(fault))
            }
            Err(oneshot::error::TryRecvError::Empty) => BrowserInputPoll::Pending,
            Err(oneshot::error::TryRecvError::Closed) => {
                self.input.take();
                BrowserInputPoll::Failed(BrowserPortFault::Preparation)
            }
        }
    }

    fn dispatch(&mut self, call: BrowserDispatch) -> BrowserDispatchOutcome {
        if self.revoked.is_some()
            || self.completion.is_some()
            || self.accepted >= NATIVE_BROWSER_TOOL_CALLS
        {
            return BrowserDispatchOutcome::Refused(call);
        }
        let Some(sender) = self.requests.as_ref() else {
            return BrowserDispatchOutcome::Refused(call);
        };
        // Capacity is acquired before consuming the move-only original key.
        // A full/closed Page channel returns the exact unchanged native call.
        let Ok(capacity) = sender.try_reserve() else {
            return BrowserDispatchOutcome::Refused(call);
        };
        let Ok((request, receiver)) = NativeBrowserToolRequest::channel(
            u32::from(call.ordinal()),
            call.name().to_owned(),
            call.arguments().clone(),
        ) else {
            return BrowserDispatchOutcome::Refused(call);
        };
        let (_, _, _, key) = call.into_parts();
        self.pending.push(PendingResult {
            key: Some(key),
            receiver,
            lost: false,
        });
        self.accepted += 1;
        capacity.send(request);
        BrowserDispatchOutcome::Accepted
    }

    fn poll_result(&mut self) -> BrowserResultPoll {
        // At most forty bounded receivers. No Page action, socket wait, user
        // interaction or async runtime drive occurs on this native thread.
        for pending in &mut self.pending {
            if pending.lost || pending.key.is_none() {
                continue;
            }
            match pending.receiver.try_recv() {
                Ok(Ok(output)) => {
                    let Some(key) = pending.key.take() else {
                        pending.lost = true;
                        continue;
                    };
                    match key.complete(output.text().to_owned(), output.success()) {
                        Ok(result) => return BrowserResultPoll::Ready(result),
                        Err(key) => {
                            pending.key = Some(key);
                            pending.lost = true;
                        }
                    }
                }
                Ok(Err(_)) | Err(oneshot::error::TryRecvError::Closed) => {
                    // An unconfirmed effect is not a synthetic success/error
                    // tool result. Retain its key until independent settlement.
                    pending.lost = true;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if self.pending.iter().any(|value| value.lost) {
            BrowserResultPoll::Lost
        } else {
            BrowserResultPoll::Pending
        }
    }

    fn budget(&self) -> Arc<dyn BrowserBudget> {
        self.timing.clone()
    }

    fn revoke(&mut self, reason: BrowserRevokeReason) {
        let should_forward = self.revoked.is_none()
            || (self.revoked == Some(BrowserRevokeReason::Cancelled)
                && reason != BrowserRevokeReason::Cancelled);
        // A Page-local receipt is not the original+local dual settlement.
        // Explicit owner/account revocation must still reach a Page caller
        // awaiting that join. Only ordinary Drop is quiet after local return.
        if should_forward {
            self.revoked = Some(reason);
            self.revoke.revoke(match reason {
                BrowserRevokeReason::ScopeChanged => NativeBrowserRevokeReason::ScopeChanged,
                BrowserRevokeReason::ConnectionLost => NativeBrowserRevokeReason::ConnectionLost,
                BrowserRevokeReason::Protocol => NativeBrowserRevokeReason::Protocol,
                BrowserRevokeReason::Cancelled => NativeBrowserRevokeReason::Cancelled,
            });
        }
        self.requests.take();
    }

    fn close_admission(&mut self, completion: BrowserCompletionKey) {
        // A normal original terminal seals future Page actions and wakes a
        // pending approval without inventing cancellation of an idle reply.
        self.revoke.close_admission();
        self.requests.take();
        if self.completion.is_some() || self.settled || self.settlement_lost {
            self.settlement_lost = true;
            return;
        }
        self.completion = Some(completion);
    }

    fn poll_local_settlement(&mut self) -> BrowserLocalSettlement {
        if self.settled || self.settlement_lost {
            return BrowserLocalSettlement::Lost;
        }
        if self.completion.is_none() {
            return BrowserLocalSettlement::Pending;
        }
        match self.settlement.try_recv() {
            Ok(_actual_private_page_receipt) => {
                let Some(completion) = self.completion.take() else {
                    self.settlement_lost = true;
                    return BrowserLocalSettlement::Lost;
                };
                self.settled = true;
                self.pending.clear();
                BrowserLocalSettlement::Observed(completion.observed())
            }
            Err(oneshot::error::TryRecvError::Empty) => BrowserLocalSettlement::Pending,
            Err(oneshot::error::TryRecvError::Closed) => {
                self.settlement_lost = true;
                BrowserLocalSettlement::Lost
            }
        }
    }
}

impl Drop for PageBrowserPort {
    fn drop(&mut self) {
        // Unexpected owner loss revokes Page's admission, not its entered
        // action future. Drop never manufactures a local-settlement receipt.
        if !self.settled {
            self.revoke(BrowserRevokeReason::ConnectionLost);
        }
    }
}

fn page_fault(fault: PageFault) -> BrowserPortFault {
    match fault {
        PageFault::Cancelled => BrowserPortFault::Cancelled,
        PageFault::ScopeChanged | PageFault::HistoryDiverged => BrowserPortFault::ScopeChanged,
        PageFault::Protocol => BrowserPortFault::Protocol,
        PageFault::Limit => BrowserPortFault::Limit,
        PageFault::Timeout => BrowserPortFault::Budget,
        PageFault::Unavailable | PageFault::Upstream => BrowserPortFault::ConnectionLost,
        PageFault::UnsupportedInput
        | PageFault::LocalInterruption
        | PageFault::OutcomeUnknown
        | PageFault::HistoryUncertain
        | PageFault::NotSent => BrowserPortFault::Preparation,
    }
}
