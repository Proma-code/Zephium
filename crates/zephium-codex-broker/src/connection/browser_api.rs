//! Native-only BrowserV1 reservation and handoff types. The preparation proof is
//! data-only: dropping it cannot send a provider frame or mutate durable history.
use super::*;
use crate::{
    browser_owner::BrowserOwner,
    browser_port::{BrowserManifest, BrowserPort, BrowserRevokeReason},
};

pub(super) struct BrowserAttempt {
    pub(super) key: ConversationKey,
    pub(super) reply: u64,
    pub(super) owner: BrowserOwner,
    pub(super) provider_entered: bool,
    pub(super) admission: TextAdmission,
    pub(super) grant: durable::Scope,
}

pub struct BrowserDraft {
    pub key: ConversationKey,
    pub new_conversation: bool,
    pub previous_reply: Option<u64>,
    pub model: String,
    pub effort: EffortSelection,
    /// Original newly visible draft, possibly empty for a suggestion-only call.
    /// Complete composed Page input is admitted later through the owned port.
    pub text: String,
}
pub struct PreparedBrowser {
    pub(super) owner: Arc<()>,
    pub(super) scope: Scope,
    pub(super) grant: durable::Scope,
    pub(super) draft: BrowserDraft,
    pub(super) choice: ModelChoice,
    pub(super) record: Option<durable::Association>,
    pub(super) manifest: Arc<BrowserManifest>,
}
pub struct BrowserCommit {
    pub reply: u64,
    pub admission: TextAdmission,
    pub port: Box<dyn BrowserPort>,
}
/// Normal failures retain the adopted bridge inside Connection. Only a call
/// made while another Browser bridge is already owned returns separate sealed
/// unadopted custody; it is kept in the SAME caller slot, never a second queue.
pub struct BrowserStartFailure {
    fault: Fault,
    unadopted: Option<Box<RejectedBrowser>>,
}
impl BrowserStartFailure {
    pub fn fault(&self) -> Fault {
        self.fault
    }
    pub fn into_parts(self) -> (Fault, Option<RejectedBrowser>) {
        (self.fault, self.unadopted.map(|value| *value))
    }
    pub(super) fn retained(fault: Fault) -> Self {
        Self {
            fault,
            unadopted: None,
        }
    }
    pub(super) fn busy(scope: Scope, proof: PreparedBrowser, commit: BrowserCommit) -> Self {
        // This branch occurs before any native operation, intent, Store write
        // or provider send for this commit. It grants no Skip/NotSent receipt.
        let mut owner = BrowserOwner::new(proof.manifest, commit.port);
        let _ = owner.close_admission(Some(BrowserRevokeReason::Protocol));
        let _ = owner.original_settled();
        Self {
            fault: Fault::Busy,
            unadopted: Some(Box::new(RejectedBrowser {
                scope,
                key: proof.draft.key,
                reply: commit.reply,
                owner,
            })),
        }
    }
}
/// Unadopted local Page obligation, never an original provider outcome. Retain
/// until its actual Page receipt; a drop/error/panic cannot release the slot.
pub struct RejectedBrowser {
    scope: Scope,
    key: ConversationKey,
    reply: u64,
    owner: BrowserOwner,
}
pub struct RejectedBrowserSettled {
    scope: Scope,
    key: ConversationKey,
    reply: u64,
}
impl RejectedBrowserSettled {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn conversation(&self) -> &ConversationKey {
        &self.key
    }
    pub fn reply(&self) -> u64 {
        self.reply
    }
}
impl RejectedBrowser {
    pub fn local_receipt_lost(&self) -> bool {
        self.owner.local_receipt_lost()
    }
    pub fn poll_settlement(&mut self) -> Result<Option<RejectedBrowserSettled>, Fault> {
        // No Page call was ever dispatched through this unadopted port. Input
        // completion or a closed channel still does not replace local receipt.
        self.owner.poll_results().map_err(|_| Fault::Protocol)?;
        self.owner
            .poll_local_settlement()
            .map_err(|_| Fault::Protocol)
            .map(|settled| {
                settled.map(|_| RejectedBrowserSettled {
                    scope: self.scope.clone(),
                    key: self.key.clone(),
                    reply: self.reply,
                })
            })
    }
}

pub(super) fn policy(manifest: &BrowserManifest) -> durable::ConversationPolicy {
    durable::ConversationPolicy::BrowserV1 {
        tool_manifest_sha256: manifest.policy().tool_digest(),
        instruction_policy_sha256: manifest.policy().instruction_digest(),
    }
}
impl Connection {
    /// Observational capability projection only; reservation and writer still
    /// repeat exact current scope/policy/admission checks independently.
    pub fn browser_capable(&self, scope: &Scope) -> bool {
        self.current(scope).is_ok()
            && self
                .browser_capability
                .as_ref()
                .zip(self.session.as_ref())
                .is_some_and(|(cap, session)| cap.qualifies(session))
    }
    pub fn can_admit_browser(&self, scope: &Scope) -> bool {
        self.browser_capable(scope)
            && self.ordinary_current(scope).is_ok()
            && self.state() == AccountState::ChatGpt
            && self.catalog.is_some()
            && self.idle().is_ok()
    }
    /// Read-only pre-say proof. Run on the existing native worker, never while
    /// holding the Page transcript lock. No intent, provider frame, history
    /// mutation or persistent actor reservation is created here.
    pub fn prepare_browser(
        &mut self,
        scope: &Scope,
        draft: BrowserDraft,
        manifest: BrowserManifest,
        cancel: &AtomicBool,
    ) -> Result<PreparedBrowser, Fault> {
        check(Instant::now() + RPC, cancel)?;
        self.ordinary_current(scope)?;
        self.idle()?;
        if !self
            .browser_capability
            .as_ref()
            .zip(self.session.as_ref())
            .is_some_and(|(cap, session)| cap.qualifies(session))
        {
            return Err(Fault::UnsupportedRuntime);
        }
        if self.state() != AccountState::ChatGpt {
            return Err(Fault::NotConnected);
        }
        if draft.text.len() > crate::browser_port::MAX_COMPOSED_INPUT_BYTES {
            return Err(Fault::UnsupportedText);
        }
        let choice = self
            .catalog
            .as_ref()
            .ok_or(Fault::NotConnected)?
            .select(&draft.model, draft.effort)
            .map_err(|_| Fault::Policy)?;
        let record = match self.associations.call(durable::Call::Load {
            scope: self.association_scope,
            policy: policy(&manifest),
            conversation: draft.key.0.clone(),
        }) {
            durable::Reply::Association(record) => record,
            durable::Reply::Failed(durable::Fault::PolicyMismatch) => {
                return Err(Fault::HistoryPolicy)
            }
            durable::Reply::Failed(durable::Fault::StaleGrant) => return Err(Fault::ScopeChanged),
            _ => return self.fail(Fault::PersistenceUnavailable),
        };
        if let Some(record) = &record {
            self.validate_record(&draft.key, record)?;
            if record.policy != policy(&manifest) {
                return Err(Fault::HistoryPolicy);
            }
            let previous = match record.dispatch {
                durable::Dispatch::Ready => None,
                durable::Dispatch::Skipped { reply } | durable::Dispatch::Settled { reply, .. } => {
                    Some(reply)
                }
                durable::Dispatch::Pending { .. } => return Err(Fault::HistoryUncertain),
            };
            if draft.previous_reply != previous {
                return Err(Fault::HistoryDiverged);
            }
            if draft.new_conversation {
                return Err(Fault::Policy);
            }
        } else {
            if draft.previous_reply.is_some() {
                return Err(Fault::HistoryDiverged);
            }
            if !draft.new_conversation {
                return Err(Fault::UnknownConversation);
            }
            if self.conversations.contains_key(&draft.key) {
                return self.fail(Fault::PersistenceUnavailable);
            }
        }
        if self
            .conversations
            .get(&draft.key)
            .is_some_and(|thread| !matches!(thread.phase(), Phase::New | Phase::Closed))
        {
            return Err(Fault::Busy);
        }
        if !self.conversations.contains_key(&draft.key)
            && self.conversations.len() >= MAX_CONVERSATIONS
        {
            return Err(Fault::Limit);
        }
        check(Instant::now() + RPC, cancel)?;
        self.ordinary_current(scope)?;
        Ok(PreparedBrowser {
            owner: self.owner_stamp.clone(),
            scope: self.scope.clone(),
            grant: self.association_scope,
            draft,
            choice,
            record,
            manifest: Arc::new(manifest),
        })
    }
    /// The committed Page bridge is adopted before fallible native admission.
    /// Refusal still owns and drains that local obligation; it does not infer a
    /// Skip or provider outcome. A preexisting owner returns separate custody.
    pub fn start_browser(
        &mut self,
        scope: &Scope,
        proof: PreparedBrowser,
        commit: BrowserCommit,
        cancel: &AtomicBool,
    ) -> Result<(), BrowserStartFailure> {
        if self.browser.is_some() {
            return Err(BrowserStartFailure::busy(self.scope.clone(), proof, commit));
        }
        let preflight = self.ordinary_current(scope).and_then(|()| self.idle());
        let BrowserCommit {
            reply,
            admission,
            port,
        } = commit;
        self.browser = Some(BrowserAttempt {
            key: proof.draft.key.clone(),
            reply,
            owner: BrowserOwner::new(proof.manifest.clone(), port),
            provider_entered: false,
            admission: admission.clone(),
            grant: self.association_scope,
        });
        let result = preflight.and_then(|()| self.admit_browser(proof, reply, admission, cancel));
        if let Err(fault) = result {
            if let Some(browser) = &mut self.browser {
                let _ = browser.owner.close_admission(Some(revoke_reason(fault)));
                if !browser.provider_entered {
                    let _ = browser.owner.original_settled();
                }
            }
            return Err(BrowserStartFailure::retained(fault));
        }
        Ok(())
    }
    fn admit_browser(
        &mut self,
        proof: PreparedBrowser,
        reply: u64,
        admission: TextAdmission,
        cancel: &AtomicBool,
    ) -> Result<(), Fault> {
        admission.check_scope()?;
        check(Instant::now() + RPC, cancel)?;
        if !Arc::ptr_eq(&proof.owner, &self.owner_stamp)
            || proof.scope != self.scope
            || proof.grant != self.association_scope
        {
            return Err(Fault::ScopeChanged);
        }
        if !self
            .browser_capability
            .as_ref()
            .zip(self.session.as_ref())
            .is_some_and(|(cap, session)| cap.qualifies(session))
        {
            return Err(Fault::UnsupportedRuntime);
        }
        if self.state() != AccountState::ChatGpt {
            return Err(Fault::NotConnected);
        }
        if reply == 0
            || reply > i64::MAX as u64
            || proof
                .draft
                .previous_reply
                .is_some_and(|prior| reply <= prior)
        {
            return Err(Fault::Policy);
        }
        // A proof cannot be reconstructed from an uncertain or different Store
        // result after the Page commit. Exact same revision/policy is required.
        match self.associations.call(durable::Call::Load {
            scope: self.association_scope,
            policy: policy(&proof.manifest),
            conversation: proof.draft.key.0.clone(),
        }) {
            durable::Reply::Association(record) if record == proof.record => (),
            durable::Reply::Failed(durable::Fault::PolicyMismatch) => {
                return Err(Fault::HistoryPolicy)
            }
            durable::Reply::Failed(durable::Fault::StaleGrant) => return Err(Fault::ScopeChanged),
            _ => return self.fail(Fault::PersistenceUnavailable),
        }
        admission.check_scope()?;
        let key = proof.draft.key;
        if !self.conversations.contains_key(&key) {
            let thread = match proof
                .record
                .as_ref()
                .and_then(|record| record.thread_id.as_ref())
            {
                Some(thread) => Conversation::from_cold_store(self.scope.clone(), thread.clone())
                    .map_err(conversation_fault)?,
                None => Conversation::new(self.scope.clone()),
            };
            self.conversations.insert(key.clone(), thread);
        }
        if let Some(record) = proof.record {
            self.remember_settled(&key, &record);
            self.records.insert(key.clone(), record);
        }
        let acknowledged_revision = self.records.get(&key).map(|record| record.revision);
        let operation = TurnOperation::browser_pending(
            self.scope.clone(),
            TurnHistory {
                incarnation: self.association_scope.incarnation,
                grant_epoch: self.association_scope.grant_epoch,
                conversation: key.0.clone(),
                reply,
                previous_reply: proof.draft.previous_reply,
                expected_revision: acknowledged_revision,
                thread_id: self
                    .records
                    .get(&key)
                    .and_then(|record| record.thread_id.clone()),
            },
            proof.choice.model(),
            proof.choice.selection(),
            &proof.draft.text,
            proof.manifest.policy(),
        )
        .ok_or(Fault::Policy)?;
        let intent = self
            .session
            .as_mut()
            .ok_or(Fault::Closed)?
            .reserve_browser_input(operation.clone())
            .map_err(transport)?;
        let deadline = self
            .browser
            .as_ref()
            .ok_or(Fault::Protocol)?
            .owner
            .hard_deadline()
            .map_err(|_| Fault::Timeout)?;
        self.prompt = Some(Prompt {
            policy: policy(&proof.manifest),
            operation,
            intent: None,
            browser_intent: Some(intent),
            unsent: None,
            acknowledged_revision,
            admission,
            reply,
            key: key.clone(),
            choice: proof.choice,
            text: None,
            deadline,
            open_deadline: deadline.min(Instant::now() + RPC),
            retry_at: None,
        });
        self.active = Some(key);
        self.notification_frames = 0;
        Ok(())
    }
    pub(super) fn browser_input(&mut self, cancel: &AtomicBool) -> Result<(), Fault> {
        if self
            .prompt
            .as_ref()
            .is_none_or(|prompt| prompt.browser_intent.is_none())
        {
            return Ok(());
        }
        self.check_prompt_scope(cancel)?;
        if self.prompt_cancelled() {
            self.reject_local_intent()?;
            return self.finish_unsent();
        }
        let input = match self
            .browser
            .as_mut()
            .ok_or(Fault::Protocol)?
            .owner
            .poll_input(Instant::now())
        {
            Ok(Some(input)) => input,
            Ok(None) => return Ok(()),
            Err(crate::browser_owner::Fault::Input) => {
                self.reject_local_intent()?;
                return self.finish_unsent();
            }
            Err(_) => return Err(Fault::Timeout),
        };
        let intent = self
            .prompt
            .as_mut()
            .and_then(|prompt| prompt.browser_intent.take())
            .ok_or(Fault::Protocol)?;
        let refined = match intent.prepare(input) {
            Ok(value) => value,
            Err((intent, _)) => {
                self.prompt.as_mut().ok_or(Fault::Protocol)?.browser_intent = Some(intent);
                self.reject_local_intent()?;
                return self.finish_unsent();
            }
        };
        let (operation, intent, text) = refined.into_parts();
        let prompt = self.prompt.as_mut().ok_or(Fault::Protocol)?;
        prompt.operation = operation;
        prompt.intent = Some(intent);
        prompt.text = Some(text.into_validated_text());
        prompt.open_deadline = prompt.deadline.min(Instant::now() + RPC);
        self.browser
            .as_mut()
            .ok_or(Fault::Protocol)?
            .provider_entered = true;
        self.open_prompt(cancel)
    }
    pub(super) fn browser_pump(
        &mut self,
        cancel: &AtomicBool,
        out: &mut Vec<ScopedEvent>,
    ) -> Result<(), Fault> {
        let Some(browser) = self.browser.as_mut() else {
            return Ok(());
        };
        if browser.owner.draining() {
            out.extend(self.poll_browser_settlement(&self.scope.clone())?);
            return Ok(());
        }
        browser.admission.check_scope()?;
        if browser.grant != self.association_scope || self.state() != AccountState::ChatGpt {
            return Err(Fault::ScopeChanged);
        }
        if self
            .browser
            .as_ref()
            .is_some_and(|browser| browser.admission.cancelled())
        {
            self.cancel_existing_turn(&self.scope.clone(), cancel)?;
            if let Some(browser) = self.browser.as_mut() {
                browser
                    .owner
                    .close_admission(Some(BrowserRevokeReason::Cancelled))
                    .map_err(|_| Fault::Protocol)?;
            }
            return Ok(());
        }
        let browser = self.browser.as_mut().ok_or(Fault::Protocol)?;
        browser
            .owner
            .deadline(Instant::now())
            .map_err(|_| Fault::Timeout)?;
        browser.owner.poll_results().map_err(|_| Fault::Protocol)?;
        let Some(binding) = browser.owner.binding().cloned() else {
            return Ok(());
        };
        let response = browser
            .owner
            .take_response(&binding, Instant::now())
            .map_err(|_| Fault::Protocol)?;
        if let Some(mut bytes) = response {
            let deadline = browser.owner.hard_deadline().map_err(|_| Fault::Timeout)?;
            let fence = browser
                .admission
                .writer_fence()
                .with_browser_budget(browser.owner.budget())
                .ok_or(Fault::Protocol)?;
            let result = self
                .session
                .as_mut()
                .ok_or(Fault::Closed)?
                .send_frame_fenced(&bytes, deadline.min(self.deadline), cancel, Some(fence))
                .map_err(transport);
            bytes.fill(0);
            result?;
        }
        self.browser
            .as_mut()
            .ok_or(Fault::Protocol)?
            .owner
            .dispatch_one(&binding, Instant::now())
            .map_err(|_| Fault::Protocol)
    }
    pub(super) fn browser_request(&mut self, bytes: &[u8]) -> Result<bool, Fault> {
        let value = wire::unique_value(bytes).map_err(|_| Fault::Protocol)?;
        if value.get("id").is_none() || value.get("method").is_none() {
            return Ok(false);
        }
        let browser = self.browser.as_mut().ok_or(Fault::UnsupportedAction)?;
        browser.admission.check_scope()?;
        if browser.admission.cancelled() || browser.grant != self.association_scope {
            return Err(Fault::ScopeChanged);
        }
        let phase = self
            .conversations
            .get(&browser.key)
            .ok_or(Fault::Protocol)?
            .phase();
        if !matches!(phase, Phase::StartingTurn | Phase::Running) {
            return Err(Fault::Protocol);
        }
        browser.owner.request(bytes).map_err(|_| Fault::Protocol)?;
        Ok(true)
    }
    pub fn browser_settlement_pending(&self) -> bool {
        self.browser.is_some()
    }
    pub fn browser_draining(&self) -> bool {
        self.browser
            .as_ref()
            .is_some_and(|browser| browser.owner.draining())
    }
    /// Retire ONLY this exact native bridge, then physically settle the original
    /// process. Local Page custody remains in Connection for factual drain.
    pub fn retire_browser(
        &mut self,
        scope: &Scope,
        reason: BrowserRevokeReason,
    ) -> Result<(), Fault> {
        if scope != &self.scope {
            return Err(Fault::ScopeChanged);
        }
        if self.browser.is_none() {
            return Err(Fault::Closed);
        }
        // A malformed local completion key cannot prevent physical retirement.
        // Keep the same port/custody, but always close the original owner below.
        let local = self
            .browser
            .as_mut()
            .ok_or(Fault::Closed)?
            .owner
            .close_admission(Some(reason))
            .map_err(|_| Fault::Protocol);
        let fault = match reason {
            BrowserRevokeReason::ScopeChanged => Fault::ScopeChanged,
            BrowserRevokeReason::ConnectionLost => Fault::Transport,
            BrowserRevokeReason::Protocol => Fault::Protocol,
            BrowserRevokeReason::Cancelled => Fault::Cancelled,
        };
        let _ = self.fail::<()>(fault);
        local
    }
    /// Closed/retired scope may poll factual local completion only. This never
    /// invokes Store, account/model RPC, dispatch, or a fresh budget/authority.
    pub fn poll_browser_settlement(&mut self, scope: &Scope) -> Result<Vec<ScopedEvent>, Fault> {
        if scope != &self.scope {
            return Err(Fault::ScopeChanged);
        }
        let Some(browser) = &mut self.browser else {
            return Ok(Vec::new());
        };
        if !browser.owner.draining() {
            return Ok(Vec::new());
        }
        // Lost individual output is not an aggregate receipt. Keep draining.
        if let Err(fault) = browser.owner.poll_results() {
            if fault != crate::browser_owner::Fault::ResultLost {
                return Err(Fault::Protocol);
            }
        }
        let settled = browser
            .owner
            .poll_local_settlement()
            .map_err(|_| Fault::Protocol)?;
        let Some(local) = settled else {
            return Ok(Vec::new());
        };
        let key = browser.key.clone();
        let reply = browser.reply;
        self.browser = None;
        Ok(vec![
            self.event(Some(key), Event::BrowserSettled { reply, local })
        ])
    }
}
pub(super) fn revoke_reason(fault: Fault) -> BrowserRevokeReason {
    match fault {
        Fault::Cancelled => BrowserRevokeReason::Cancelled,
        Fault::ScopeChanged | Fault::NotConnected => BrowserRevokeReason::ScopeChanged,
        Fault::Transport | Fault::Closed | Fault::Timeout => BrowserRevokeReason::ConnectionLost,
        _ => BrowserRevokeReason::Protocol,
    }
}

/// Closed original owner with unresolved local custody. No action or recovery
/// methods are exposed; retaining it keeps the exact bridge allocation alive.
pub struct BrowserQuarantine {
    _connection: Connection,
}
impl Connection {
    pub fn browser_local_receipt_lost(&self) -> bool {
        self.browser
            .as_ref()
            .is_some_and(|browser| browser.owner.local_receipt_lost())
    }
    pub fn quarantine_browser(self) -> Result<BrowserQuarantine, Box<Self>> {
        if self.session.is_some() || !self.browser_local_receipt_lost() {
            return Err(Box::new(self));
        }
        Ok(BrowserQuarantine { _connection: self })
    }
}
#[cfg(test)]
mod custody_traits {
    use super::*;
    fn send<T: Send>() {}
    #[test]
    fn closed_custody_can_move_without_cloning_or_granting_authority() {
        send::<Connection>();
        send::<RejectedBrowser>();
        send::<BrowserQuarantine>();
        send::<PreparedBrowser>();
    }
}
