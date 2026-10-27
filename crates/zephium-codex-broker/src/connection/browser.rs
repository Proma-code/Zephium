//! Nonblocking native dynamic-tool/local-effect owner. Original process and
//! durable conversation receipts remain Connection-owned. This module cannot
//! run a Page action, start/resend a model turn, or renew the Page budget.
use crate::{
    browser_port::{
        BrowserBudget, BrowserCompletionKey, BrowserDispatch, BrowserDispatchOutcome, BrowserInput,
        BrowserInputPoll, BrowserLocalSettlement, BrowserManifest, BrowserPort, BrowserResultPoll,
        BrowserRevokeReason, BrowserStamp,
    },
    dynamic_tools::{
        self, EarlyRequests, EffectOutcome, EffectTicket, LifecycleLedger, ResponseTicket,
        ToolRequest, TurnBinding, TurnLedger,
    },
};
use std::{sync::Arc, time::Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Protocol,
    Budget,
    Revoked,
    Input,
    PageRefused,
    ResultLost,
    LocalReceiptLost,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalOutcome {
    Observed,
    /// Actual aggregate Page settlement, but one individual result was lost.
    /// This is NOT an unknown provider outcome or proof that no action happened.
    ResultUnobserved,
}

pub(crate) struct BrowserOwner {
    manifest: Arc<BrowserManifest>,
    port: Box<dyn BrowserPort>,
    budget: Arc<dyn BrowserBudget>,
    stamp: BrowserStamp,
    early: Option<EarlyRequests>,
    binding: Option<TurnBinding>,
    ledger: Option<TurnLedger>,
    lifecycle: LifecycleLedger,
    text_ids: std::collections::BTreeSet<(String, String, String)>,
    effect: Option<EffectTicket>,
    response: Option<ResponseTicket>,
    accepted: Vec<(u8, [u8; 32])>,
    input_finished: bool,
    admission_closed: bool,
    original_settled: bool,
    local_settled: Option<LocalOutcome>,
    local_receipt_lost: bool,
    delivered: bool,
    revocation: Option<BrowserRevokeReason>,
    result_lost: bool,
}
impl BrowserOwner {
    pub(crate) fn new(manifest: Arc<BrowserManifest>, port: Box<dyn BrowserPort>) -> Self {
        let budget = port.budget();
        Self {
            manifest,
            budget,
            port,
            stamp: BrowserStamp::new(),
            early: Some(EarlyRequests::new()),
            binding: None,
            ledger: None,
            lifecycle: LifecycleLedger::new(),
            text_ids: std::collections::BTreeSet::new(),
            effect: None,
            response: None,
            accepted: Vec::new(),
            input_finished: false,
            admission_closed: false,
            original_settled: false,
            local_settled: None,
            local_receipt_lost: false,
            delivered: false,
            revocation: None,
            result_lost: false,
        }
    }
    pub(crate) fn local_receipt_lost(&self) -> bool {
        self.local_receipt_lost
    }
    pub(crate) fn manifest(&self) -> &BrowserManifest {
        &self.manifest
    }
    pub(crate) fn budget(&self) -> Arc<dyn BrowserBudget> {
        self.budget.clone()
    }
    pub(crate) fn hard_deadline(&self) -> Result<Instant, Fault> {
        self.budget.hard_deadline().map_err(|_| Fault::Budget)
    }
    pub(crate) fn deadline(&self, now: Instant) -> Result<Instant, Fault> {
        let deadline = self.budget.deadline().map_err(|_| Fault::Budget)?;
        (now < deadline).then_some(deadline).ok_or(Fault::Budget)
    }
    pub(crate) fn poll_input(&mut self, now: Instant) -> Result<Option<BrowserInput>, Fault> {
        if self.admission_closed || self.input_finished {
            return Err(Fault::Revoked);
        }
        self.deadline(now)?;
        match self.port.poll_input() {
            BrowserInputPoll::Pending => Ok(None),
            BrowserInputPoll::Ready(input) => {
                self.input_finished = true;
                Ok(Some(input))
            }
            BrowserInputPoll::Failed(_) => {
                self.input_finished = true;
                Err(Fault::Input)
            }
        }
    }
    pub(crate) fn request(&mut self, bytes: &[u8]) -> Result<(), Fault> {
        if self.admission_closed {
            return Err(Fault::Revoked);
        }
        let request = ToolRequest::decode(bytes, self.manifest.tools()).map_err(protocol)?;
        let (thread, turn, call) = request.identity_key();
        if self
            .text_ids
            .contains(&(thread.into(), turn.into(), call.into()))
        {
            return Err(Fault::Protocol);
        }
        self.lifecycle.request(&request).map_err(protocol)?;
        if let Some(ledger) = &mut self.ledger {
            ledger.admit(request).map_err(protocol)
        } else {
            self.early
                .as_mut()
                .ok_or(Fault::Protocol)?
                .push(request)
                .map_err(protocol)
        }
    }
    pub(crate) fn observe_item_identity(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<(), Fault> {
        if !method.starts_with("item/") {
            return Ok(());
        }
        let Some(thread) = params.get("threadId").and_then(serde_json::Value::as_str) else {
            return Ok(());
        };
        let Some(turn) = params.get("turnId").and_then(serde_json::Value::as_str) else {
            return Ok(());
        };
        let item = params.get("item");
        let Some(id) = item
            .and_then(|item| item.get("id"))
            .or_else(|| params.get("itemId"))
            .and_then(serde_json::Value::as_str)
        else {
            return Ok(());
        };
        let key = (thread.to_owned(), turn.to_owned(), id.to_owned());
        if item
            .and_then(|item| item.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("dynamicToolCall")
        {
            if self.text_ids.contains(&key) {
                return Err(Fault::Protocol);
            }
        } else {
            if self.lifecycle.contains(thread, turn, id) {
                return Err(Fault::Protocol);
            }
            if self.text_ids.len() >= crate::conversation::MAX_ITEMS
                && !self.text_ids.contains(&key)
            {
                return Err(Fault::Protocol);
            }
            self.text_ids.insert(key);
        }
        Ok(())
    }
    pub(crate) fn project_terminal(&self, params: &mut serde_json::Value) -> Result<(), Fault> {
        let thread = params
            .get("threadId")
            .and_then(serde_json::Value::as_str)
            .ok_or(Fault::Protocol)?
            .to_owned();
        let turn = params
            .pointer("/turn/id")
            .and_then(serde_json::Value::as_str)
            .ok_or(Fault::Protocol)?
            .to_owned();
        let Some(items) = params
            .pointer_mut("/turn/items")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return Ok(());
        };
        if items.len() > crate::conversation::MAX_ITEMS {
            return Err(Fault::Protocol);
        }
        let mut seen = std::collections::BTreeSet::new();
        for item in items.iter() {
            if !seen.insert(
                item.get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(Fault::Protocol)?,
            ) {
                return Err(Fault::Protocol);
            }
            if item.get("type").and_then(serde_json::Value::as_str) == Some("dynamicToolCall") {
                if self.text_ids.contains(&(
                    thread.clone(),
                    turn.clone(),
                    item["id"].as_str().ok_or(Fault::Protocol)?.to_owned(),
                )) {
                    return Err(Fault::Protocol);
                }
                self.lifecycle
                    .terminal_item(&thread, &turn, item, self.manifest.tools())
                    .map_err(protocol)?;
            } else if self.lifecycle.contains(
                &thread,
                &turn,
                item["id"].as_str().ok_or(Fault::Protocol)?,
            ) {
                return Err(Fault::Protocol);
            }
        }
        items.retain(|item| {
            item.get("type").and_then(serde_json::Value::as_str) != Some("dynamicToolCall")
        });
        Ok(())
    }
    pub(crate) fn late_lifecycle(
        &self,
        method: &str,
        params: &serde_json::Value,
        thread: &str,
        turn: &str,
    ) -> Result<bool, Fault> {
        LifecycleLedger::tail(method, params, self.manifest.tools(), thread, turn).map_err(protocol)
    }
    pub(crate) fn lifecycle(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<bool, Fault> {
        self.lifecycle
            .notification(method, params, self.manifest.tools())
            .map_err(protocol)
    }
    /// The actor supplies only the original actual turn/start receipt join.
    pub(crate) fn bind(&mut self, binding: TurnBinding) -> Result<(), Fault> {
        if self.binding.is_some() || self.admission_closed {
            return Err(Fault::Protocol);
        }
        self.lifecycle.bind(binding.clone()).map_err(protocol)?;
        let ledger = self
            .early
            .take()
            .ok_or(Fault::Protocol)?
            .join(binding.clone())
            .map_err(protocol)?;
        self.binding = Some(binding);
        self.ledger = Some(ledger);
        Ok(())
    }
    /// Called only after the native current scope/account/call fence, and after
    /// reducing the pump's account/terminal notifications. The Page owner repeats
    /// its own actual authority checks before entering the existing dispatcher.
    pub(crate) fn dispatch_one(
        &mut self,
        current: &TurnBinding,
        now: Instant,
    ) -> Result<(), Fault> {
        if self.admission_closed {
            return Err(Fault::Revoked);
        }
        self.deadline(now)?; // reread, never a cached human-wait extension
        if self.effect.is_some() || self.response.is_some() {
            return Ok(());
        }
        let ledger = self.ledger.as_mut().ok_or(Fault::Protocol)?;
        let Some(ticket) = ledger.claim(current).map_err(protocol)? else {
            return Ok(());
        };
        let ordinal = ticket.ordinal();
        let digest = ticket.body_digest();
        let call = BrowserDispatch::new(
            self.stamp.clone(),
            ordinal,
            ticket.tool(),
            ticket.arguments().clone(),
            digest,
        )
        .ok_or(Fault::Protocol)?;
        let effect = ledger.start(ticket, current).map_err(protocol)?;
        // Keep the obligation before handing the Page future any work. A failed
        // typed refusal join must leave it retained rather than imply no effect.
        self.effect = Some(effect);
        match self.port.dispatch(call) {
            BrowserDispatchOutcome::Accepted => {
                self.accepted.push((ordinal, digest));
                Ok(())
            }
            BrowserDispatchOutcome::Refused(call) => {
                if !call.matches(&self.stamp, ordinal, digest) {
                    // A corrupted adapter refusal cannot establish whether the
                    // attempted call entered its owner. Do not give an aggregate
                    // receipt permission to discharge an unrecorded effect.
                    self.local_receipt_lost = true;
                    return Err(Fault::Protocol);
                }
                let effect = self.effect.take().ok_or(Fault::Protocol)?;
                ledger
                    .rejected_before_page_admission(effect)
                    .map_err(protocol)?;
                Err(Fault::PageRefused)
            }
        }
    }
    /// Poll factual local results even after revocation/terminal. No send occurs
    /// here; the original scoped response ticket is handed off separately once.
    pub(crate) fn poll_results(&mut self) -> Result<(), Fault> {
        match self.port.poll_result() {
            BrowserResultPoll::Pending => Ok(()),
            BrowserResultPoll::Lost => {
                if self.effect.is_some() {
                    self.result_lost = true;
                    Err(Fault::ResultLost)
                } else {
                    Err(Fault::Protocol)
                }
            }
            BrowserResultPoll::Ready(result) => {
                let effect = self.effect.as_ref().ok_or(Fault::Protocol)?;
                let (text, success) = result
                    .consume(&self.stamp, effect.ordinal(), effect.body_digest())
                    .map_err(|_| Fault::Protocol)?;
                let effect = self.effect.take().ok_or(Fault::Protocol)?;
                let response = self
                    .ledger
                    .as_mut()
                    .ok_or(Fault::Protocol)?
                    .settle(effect, EffectOutcome::Text { text, success })
                    .map_err(protocol)?;
                if self.response.is_some() {
                    return Err(Fault::Protocol);
                }
                self.response = response;
                Ok(())
            }
        }
    }
    pub(crate) fn take_response(
        &mut self,
        current: &TurnBinding,
        now: Instant,
    ) -> Result<Option<Vec<u8>>, Fault> {
        if self.admission_closed {
            return Ok(None);
        }
        self.deadline(now)?;
        let Some(response) = self.response.take() else {
            return Ok(None);
        };
        self.ledger
            .as_mut()
            .ok_or(Fault::Protocol)?
            .handoff(response, current)
            .map(Some)
            .map_err(protocol)
    }
    /// Normal authoritative terminal closes admission only. Cancellation, scope
    /// loss and native failure additionally revoke, including during local drain.
    pub(crate) fn close_admission(
        &mut self,
        reason: Option<BrowserRevokeReason>,
    ) -> Result<(), Fault> {
        if self.delivered {
            return Ok(());
        }
        if let Some(reason) = reason {
            if self.revocation.is_none()
                || (self.revocation == Some(BrowserRevokeReason::Cancelled)
                    && reason != BrowserRevokeReason::Cancelled)
            {
                self.port.revoke(reason);
                self.revocation = Some(reason);
            }
        }
        if !self.admission_closed {
            self.admission_closed = true;
            self.lifecycle.retire();
            if let Some(ledger) = &mut self.ledger {
                ledger.retire();
            }
            self.early = None;
            self.response = None;
            let completion = BrowserCompletionKey::new(self.stamp.clone(), self.accepted.clone())
                .ok_or(Fault::Protocol)?;
            self.port.close_admission(completion);
        }
        Ok(())
    }
    /// Called only after the real unsubscribe+thread/closed barrier or physical
    /// process/job/writer settlement. It is not inferred from turn/completed.
    pub(crate) fn original_settled(&mut self) -> Result<(), Fault> {
        if !self.admission_closed {
            return Err(Fault::Protocol);
        }
        self.original_settled = true;
        Ok(())
    }
    /// This drain is allowed after all admission/deadline/account authority is
    /// retired. It can neither dispatch nor send a response. Lost receipts keep
    /// quarantine; a timer or dropped channel does not convert them to success.
    pub(crate) fn poll_local_settlement(&mut self) -> Result<Option<LocalOutcome>, Fault> {
        if self.delivered {
            return Ok(None);
        }
        if !self.admission_closed {
            return Ok(None);
        }
        if self.local_settled.is_none() && !self.local_receipt_lost {
            match self.port.poll_local_settlement() {
                BrowserLocalSettlement::Pending => {}
                BrowserLocalSettlement::Lost => {
                    self.local_receipt_lost = true;
                    return Err(Fault::LocalReceiptLost);
                }
                BrowserLocalSettlement::Observed(receipt) => {
                    if receipt.consume(&self.stamp, &self.accepted).is_err() {
                        self.local_receipt_lost = true;
                        return Err(Fault::Protocol);
                    }
                    if let Some(effect) = self.effect.take() {
                        // Actual Page aggregate settlement proves the recorded
                        // accepted local future returned, not its unobserved result.
                        self.ledger
                            .as_mut()
                            .ok_or(Fault::Protocol)?
                            .settle(effect, EffectOutcome::Unknown)
                            .map_err(protocol)?;
                        self.result_lost = true;
                    }
                    if self
                        .ledger
                        .as_ref()
                        .is_some_and(|ledger| !ledger.local_effects_settled())
                    {
                        return Err(Fault::Protocol);
                    }
                    self.local_settled = Some(if self.result_lost {
                        LocalOutcome::ResultUnobserved
                    } else {
                        LocalOutcome::Observed
                    });
                }
            }
        }
        if self.original_settled {
            if let Some(outcome) = self.local_settled {
                self.delivered = true;
                return Ok(Some(outcome));
            }
        }
        Ok(None)
    }
    pub(crate) fn draining(&self) -> bool {
        self.admission_closed && !self.delivered
    }
    pub(crate) fn binding(&self) -> Option<&TurnBinding> {
        self.binding.as_ref()
    }
}
fn protocol(_: dynamic_tools::Fault) -> Fault {
    Fault::Protocol
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;
