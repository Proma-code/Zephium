use super::*;
#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Queued,
    Claimed,
    Running,
    Completed,
    ResponseHandedOff,
    NotStarted,
    Unknown,
}
struct Entry {
    ordinal: u8,
    call: String,
    digest: [u8; 32],
    phase: Phase,
}
/// Buffer only before the actual original turn/start reply. No dispatch API.
pub(crate) struct EarlyRequests {
    requests: Vec<ToolRequest>,
    bytes: usize,
}
impl EarlyRequests {
    pub(crate) fn new() -> Self {
        Self {
            requests: Vec::new(),
            bytes: 0,
        }
    }
    pub(crate) fn push(&mut self, request: ToolRequest) -> Result<(), Fault> {
        if self.requests.len() >= MAX_QUEUE
            || self
                .bytes
                .checked_add(request.bytes)
                .is_none_or(|n| n > MAX_QUEUE_BYTES)
        {
            return Err(Fault::Limit);
        }
        if self
            .requests
            .iter()
            .any(|r| r.id == request.id || r.call == request.call)
        {
            return Err(Fault::Duplicate);
        }
        self.bytes += request.bytes;
        self.requests.push(request);
        Ok(())
    }
    pub(crate) fn join(self, binding: TurnBinding) -> Result<TurnLedger, Fault> {
        if self.requests.iter().any(|r| !binding.matches(r)) {
            return Err(Fault::Scope);
        }
        let mut ledger = TurnLedger::new(binding);
        for request in self.requests {
            ledger.admit(request)?;
        }
        Ok(ledger)
    }
}
/// Only the owning actor mutates this finite ledger. Tickets carry a private
/// allocation identity; another ledger with identical strings cannot adopt them.
pub(crate) struct TurnLedger {
    stamp: Arc<()>,
    binding: TurnBinding,
    live: bool,
    entries: BTreeMap<RpcId, Entry>,
    calls: BTreeSet<String>,
    queue: VecDeque<ToolRequest>,
    queue_bytes: usize,
}
pub(crate) struct ExecutionTicket {
    ordinal: u8,
    stamp: Arc<()>,
    binding: TurnBinding,
    request: ToolRequest,
}
pub(crate) struct EffectTicket {
    stamp: Arc<()>,
    binding: TurnBinding,
    ordinal: u8,
    request: ToolRequest,
}
pub(crate) struct ResponseTicket {
    stamp: Arc<()>,
    binding: TurnBinding,
    id: RpcId,
    digest: [u8; 32],
    bytes: Vec<u8>,
}
/// Returned only after the existing Page owner actually settles its tool future.
/// Unknown does not fabricate a provider success or a no-effect disposition.
pub(crate) enum EffectOutcome {
    Text { text: String, success: bool },
    Unknown,
}
impl ExecutionTicket {
    pub(crate) fn ordinal(&self) -> u8 {
        self.ordinal
    }
    pub(crate) fn tool(&self) -> PageTool {
        self.request.tool
    }
    pub(crate) fn arguments(&self) -> &Value {
        &self.request.arguments
    }
    pub(crate) fn body_digest(&self) -> [u8; 32] {
        self.request.body_digest
    }
}
impl EffectTicket {
    pub(crate) fn ordinal(&self) -> u8 {
        self.ordinal
    }
    #[cfg(test)]
    pub(crate) fn tool(&self) -> PageTool {
        self.request.tool
    }
    #[cfg(test)]
    pub(crate) fn arguments(&self) -> &Value {
        &self.request.arguments
    }
    pub(crate) fn body_digest(&self) -> [u8; 32] {
        self.request.body_digest
    }
}
impl TurnLedger {
    pub(crate) fn new(binding: TurnBinding) -> Self {
        Self {
            stamp: Arc::new(()),
            binding,
            live: true,
            entries: BTreeMap::new(),
            calls: BTreeSet::new(),
            queue: VecDeque::new(),
            queue_bytes: 0,
        }
    }
    fn current(&self, current: &TurnBinding) -> Result<(), Fault> {
        if !self.live {
            Err(Fault::Retired)
        } else if &self.binding != current {
            Err(Fault::Scope)
        } else {
            Ok(())
        }
    }
    pub(crate) fn admit(&mut self, request: ToolRequest) -> Result<(), Fault> {
        if !self.live {
            return Err(Fault::Retired);
        }
        if !self.binding.matches(&request) {
            return Err(Fault::Scope);
        }
        if self.entries.contains_key(&request.id) || self.calls.contains(&request.call) {
            return Err(Fault::Duplicate);
        }
        if self.entries.len() >= MAX_CALLS
            || self.queue.len() >= MAX_QUEUE
            || self
                .queue_bytes
                .checked_add(request.bytes)
                .is_none_or(|n| n > MAX_QUEUE_BYTES)
        {
            return Err(Fault::Limit);
        }
        self.entries.insert(
            request.id.clone(),
            Entry {
                ordinal: (self.entries.len() + 1) as u8,
                call: request.call.clone(),
                digest: request.body_digest,
                phase: Phase::Queued,
            },
        );
        self.calls.insert(request.call.clone());
        self.queue_bytes += request.bytes;
        self.queue.push_back(request);
        Ok(())
    }
    pub(crate) fn claim(
        &mut self,
        current: &TurnBinding,
    ) -> Result<Option<ExecutionTicket>, Fault> {
        self.current(current)?;
        // One effect/response obligation at a time. No model assumption that
        // dynamic calls are serial: queued requests are explicitly bounded.
        if self
            .entries
            .values()
            .any(|e| matches!(e.phase, Phase::Claimed | Phase::Running | Phase::Completed))
        {
            return Err(Fault::Busy);
        }
        let Some(request) = self.queue.pop_front() else {
            return Ok(None);
        };
        self.queue_bytes -= request.bytes;
        self.entries
            .get_mut(&request.id)
            .ok_or(Fault::Ticket)?
            .phase = Phase::Claimed;
        Ok(Some(ExecutionTicket {
            ordinal: self.entries.get(&request.id).ok_or(Fault::Ticket)?.ordinal,
            stamp: self.stamp.clone(),
            binding: self.binding.clone(),
            request,
        }))
    }
    /// Invoke at the existing Page dispatch gate after parsing/revalidation,
    /// never in response to item/started. This does not replace Page authority.
    pub(crate) fn start(
        &mut self,
        ticket: ExecutionTicket,
        current: &TurnBinding,
    ) -> Result<EffectTicket, Fault> {
        self.current(current)?;
        if !Arc::ptr_eq(&ticket.stamp, &self.stamp) || ticket.binding != self.binding {
            return Err(Fault::Ticket);
        }
        let entry = self
            .entries
            .get_mut(&ticket.request.id)
            .ok_or(Fault::Ticket)?;
        if entry.phase != Phase::Claimed
            || entry.ordinal != ticket.ordinal
            || entry.digest != ticket.request.body_digest
            || entry.call != ticket.request.call
        {
            return Err(Fault::Ticket);
        }
        entry.phase = Phase::Running;
        Ok(EffectTicket {
            stamp: ticket.stamp,
            binding: ticket.binding,
            ordinal: ticket.ordinal,
            request: ticket.request,
        })
    }
    /// Completion remains joinable after provider terminal/cancel, solely to
    /// discharge the already-started local effect. It cannot send a late reply.
    pub(crate) fn settle(
        &mut self,
        ticket: EffectTicket,
        outcome: EffectOutcome,
    ) -> Result<Option<ResponseTicket>, Fault> {
        if !Arc::ptr_eq(&ticket.stamp, &self.stamp) || ticket.binding != self.binding {
            return Err(Fault::Ticket);
        }
        let entry = self
            .entries
            .get_mut(&ticket.request.id)
            .ok_or(Fault::Ticket)?;
        if entry.phase != Phase::Running || entry.digest != ticket.request.body_digest {
            return Err(Fault::Ticket);
        }
        match outcome {
            EffectOutcome::Unknown => {
                entry.phase = Phase::Unknown;
                self.retire();
                Ok(None)
            }
            EffectOutcome::Text { text, success } => {
                entry.phase = Phase::Completed;
                if !self.live {
                    return Ok(None);
                }
                if text.len() > MAX_RESULT_BYTES {
                    return Err(Fault::Limit);
                }
                let response = json!({"id":ticket.request.id.value(),"result":{"contentItems":[{"type":"inputText","text":text}],"success":success}});
                let bytes = bounded_json(&response, MAX_FRAME_BYTES)?;
                Ok(Some(ResponseTicket {
                    stamp: ticket.stamp,
                    binding: ticket.binding,
                    id: ticket.request.id,
                    digest: ticket.request.body_digest,
                    bytes,
                }))
            }
        }
    }
    /// Only an unchanged call returned by the trusted Page channel's pre-admit
    /// refusal can take this path. The owner validates that receipt first. An
    /// accepted/lost result never permits this transition.
    pub(crate) fn rejected_before_page_admission(
        &mut self,
        ticket: EffectTicket,
    ) -> Result<(), Fault> {
        if !Arc::ptr_eq(&ticket.stamp, &self.stamp) || ticket.binding != self.binding {
            return Err(Fault::Ticket);
        }
        let entry = self
            .entries
            .get_mut(&ticket.request.id)
            .ok_or(Fault::Ticket)?;
        if entry.phase != Phase::Running || entry.digest != ticket.request.body_digest {
            return Err(Fault::Ticket);
        }
        entry.phase = Phase::NotStarted;
        self.retire();
        Ok(())
    }
    /// Consumes the only response handoff. Any uncertain/failed native write is
    /// non-retryable; it never mints another tool execution or response ticket.
    pub(crate) fn handoff(
        &mut self,
        ticket: ResponseTicket,
        current: &TurnBinding,
    ) -> Result<Vec<u8>, Fault> {
        self.current(current)?;
        if !Arc::ptr_eq(&ticket.stamp, &self.stamp) || ticket.binding != self.binding {
            return Err(Fault::Ticket);
        }
        let entry = self.entries.get_mut(&ticket.id).ok_or(Fault::Ticket)?;
        if entry.phase != Phase::Completed || entry.digest != ticket.digest {
            return Err(Fault::Ticket);
        }
        entry.phase = Phase::ResponseHandedOff;
        Ok(ticket.bytes)
    }
    pub(crate) fn retire(&mut self) {
        self.live = false;
        self.queue.clear();
        self.queue_bytes = 0;
        for entry in self.entries.values_mut() {
            if matches!(entry.phase, Phase::Queued | Phase::Claimed) {
                entry.phase = Phase::NotStarted;
            }
        }
    }
    /// False remains false if an effect ticket/waiter was dropped. Only actual
    /// owner completion/explicit Unknown can discharge a Running entry.
    pub(crate) fn local_effects_settled(&self) -> bool {
        !self.entries.values().any(|e| e.phase == Phase::Running)
    }
    #[cfg(test)]
    pub(crate) fn admitted(&self) -> usize {
        self.entries.len()
    }
}
