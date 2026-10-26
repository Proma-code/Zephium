//! Pure, single-connection text conversation reducer for original Codex 0.164.0.
//!
//! The native actor owns process/account authorization and fixed request policy.
//! These commands neither perform I/O nor accept arbitrary RPC/config/path data.
//! Every prompt needs a fresh start or *observed* same-ID cold resume; a warm
//! thread must not silently retain another prompt's skill-denial configuration.
use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::catalog::ModelChoice;

const MAX_TEXT: usize = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 1024;
const MAX_EVENTS: usize = 100_000;
const MAX_EARLY_EVENTS: usize = 128;

/// Opaque native correlation, not persisted authority or a renderer capability.
#[derive(Clone, Eq, PartialEq)]
pub struct Scope {
    profile: String,
    connection: [u8; 16],
    generation: u64,
}
impl Scope {
    pub fn new(profile: String, connection: [u8; 16], generation: u64) -> Result<Self, Fault> {
        if !identifier(&profile) || connection == [0; 16] || generation == 0 {
            return Err(Fault::InvalidInput);
        }
        Ok(Self {
            profile,
            connection,
            generation,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    InvalidInput,
    ScopeChanged,
    Busy,
    State,
    RequestId,
    Protocol,
    Limit,
    UnsupportedAction,
    Upstream,
    OutcomeUnknown,
}

/// Only the native actor can turn these into a request using its fixed policy.
/// In particular, StartTurn has no arbitrary tool/input/config injection field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    StartThread {
        request_id: u64,
        choice: ModelChoice,
    },
    ResumeThread {
        request_id: u64,
        thread_id: String,
        choice: ModelChoice,
    },
    StartTurn {
        request_id: u64,
        thread_id: String,
        choice: ModelChoice,
    },
    Interrupt {
        request_id: u64,
        thread_id: String,
        turn_id: String,
    },
    Unsubscribe {
        request_id: u64,
        thread_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    New,
    Opening,
    Ready,
    StartingTurn,
    Running,
    Closing,
    Closed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessagePhase {
    Commentary,
    PartialAnswer,
    FinalAnswer,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub id: String,
    pub text: String,
    pub phase: MessagePhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Completed,
    Interrupted,
    Failed,
    /// No matching original terminal receipt. Never a synthetic completion.
    Unknown,
}

/// Original notification counters. `last` is NOT certified per-turn usage.
/// No prices, subscription entitlement, remaining quota or invented zero usage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Usage {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageSnapshot {
    pub total: Usage,
    pub last: Usage,
    pub model_context_window: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnResult {
    pub thread_id: String,
    pub turn_id: String,
    pub outcome: Outcome,
    /// Local cancellation intent is separate from the original terminal status.
    pub cancel_requested: bool,
    pub messages: Vec<Message>,
    pub usage: Option<UsageSnapshot>,
}
impl TurnResult {
    /// Same phase preference as the pinned original SDK; commentary is not a
    /// fabricated final answer. Empty valid turns remain empty valid turns.
    pub fn final_response(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.phase == MessagePhase::FinalAnswer)
            .or_else(|| {
                self.messages
                    .iter()
                    .rev()
                    .find(|m| m.phase == MessagePhase::Unknown)
            })
            .map(|m| m.text.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    ThreadReady {
        thread_id: String,
        generation: u64,
    },
    TurnStarted {
        turn_id: String,
    },
    Delta {
        item_id: String,
        text: String,
    },
    ItemCompleted(Message),
    /// Original runtime telemetry, not permission for the broker to resend.
    /// Keep waiting for the matching terminal under the original deadline.
    ProviderError {
        will_retry: bool,
    },
    Terminal(TurnResult),
    /// The actor allocates an ID and sends a fixed turn/interrupt request.
    InterruptRequired {
        thread_id: String,
        turn_id: String,
    },
    /// Emitted only after unsubscribe receipt AND actual matching closure.
    Closed {
        thread_id: String,
        generation: u64,
    },
    TransportLost {
        thread_id: Option<String>,
        turn_id: Option<String>,
    },
}

enum Pending {
    Open { resume: bool },
    Turn,
    Interrupt,
    Unsubscribe,
}

struct Active {
    id: String,
    items: BTreeMap<String, Item>,
    order: Vec<String>,
    text_bytes: usize,
    usage: Option<UsageSnapshot>,
}
struct Item {
    delta: String,
    completed: Option<Message>,
}

pub struct Conversation {
    scope: Scope,
    phase: Phase,
    thread: Option<String>,
    choice: Option<ModelChoice>,
    generation: u64,
    last_request: u64,
    pending: BTreeMap<u64, Pending>,
    active: Option<Active>,
    seen_turns: HashSet<String>,
    consumed: bool,
    cancel: bool,
    interrupt_sent: bool,
    close_seen: bool,
    unsubscribe_ack: bool,
    resume_attempts: u8,
    early: Vec<(String, Value)>,
    early_bytes: usize,
    events: usize,
}

impl Conversation {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            phase: Phase::New,
            thread: None,
            choice: None,
            generation: 0,
            last_request: 0,
            pending: BTreeMap::new(),
            active: None,
            seen_turns: HashSet::new(),
            consumed: false,
            cancel: false,
            interrupt_sent: false,
            close_seen: false,
            unsubscribe_ack: false,
            resume_attempts: 0,
            early: Vec::new(),
            early_bytes: 0,
            events: 0,
        }
    }
    /// Native-only cold restore after a new physical process and a current
    /// profile/grant-bound Ready or original-terminal Settled Store association.
    /// Pending/unknown outcomes are not admitted. Not a renderer capability.
    /// No turn, terminal, unsubscribe ACK or thread/closed event is fabricated.
    #[cfg(any(
        test,
        all(
            windows,
            any(feature = "native-fixtures", feature = "trial-integration")
        )
    ))]
    pub(crate) fn from_cold_store(scope: Scope, thread_id: String) -> Result<Self, Fault> {
        if !identifier(&thread_id) {
            return Err(Fault::InvalidInput);
        }
        let mut conversation = Self::new(scope);
        conversation.thread = Some(thread_id);
        conversation.phase = Phase::Closed;
        Ok(conversation)
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn thread_id(&self) -> Option<&str> {
        self.thread.as_deref()
    }
    pub fn active_turn_id(&self) -> Option<&str> {
        self.active.as_ref().map(|t| t.id.as_str())
    }
    fn scope(&self, scope: &Scope) -> Result<(), Fault> {
        if self.scope == *scope {
            Ok(())
        } else {
            Err(Fault::ScopeChanged)
        }
    }
    fn request(&mut self, id: u64, pending: Pending) -> Result<(), Fault> {
        if id == 0 || id <= self.last_request {
            return Err(Fault::RequestId);
        }
        self.last_request = id;
        self.pending.insert(id, pending);
        Ok(())
    }
    fn poison<T>(&mut self, fault: Fault) -> Result<T, Fault> {
        self.phase = Phase::Failed;
        self.early.clear();
        Err(fault)
    }
    pub fn begin_start(
        &mut self,
        scope: &Scope,
        id: u64,
        choice: ModelChoice,
    ) -> Result<Command, Fault> {
        self.scope(scope)?;
        if self.phase != Phase::New {
            return Err(Fault::State);
        }
        self.request(id, Pending::Open { resume: false })?;
        self.choice = Some(choice.clone());
        self.phase = Phase::Opening;
        Ok(Command::StartThread {
            request_id: id,
            choice,
        })
    }
    pub fn begin_resume(
        &mut self,
        scope: &Scope,
        id: u64,
        choice: ModelChoice,
    ) -> Result<Command, Fault> {
        self.scope(scope)?;
        if self.phase != Phase::Closed || !self.pending.is_empty() {
            return Err(Fault::State);
        }
        if self.resume_attempts >= 3 {
            return self.poison(Fault::Limit);
        }
        let thread_id = self.thread.clone().ok_or(Fault::State)?;
        self.request(id, Pending::Open { resume: true })?;
        self.resume_attempts += 1;
        self.choice = Some(choice.clone());
        self.phase = Phase::Opening;
        Ok(Command::ResumeThread {
            request_id: id,
            thread_id,
            choice,
        })
    }
    pub fn begin_turn(&mut self, scope: &Scope, id: u64) -> Result<Command, Fault> {
        self.scope(scope)?;
        if self.phase != Phase::Ready || self.consumed || !self.pending.is_empty() {
            return Err(Fault::State);
        }
        let thread_id = self.thread.clone().ok_or(Fault::State)?;
        let choice = self.choice.clone().ok_or(Fault::State)?;
        self.request(id, Pending::Turn)?;
        self.phase = Phase::StartingTurn;
        self.consumed = true;
        self.cancel = false;
        self.interrupt_sent = false;
        self.events = 0;
        Ok(Command::StartTurn {
            request_id: id,
            thread_id,
            choice,
        })
    }
    /// Cancellation while turn/start is pending is latched, not resent. Once
    /// its actual ID is known the reducer asks the actor to interrupt that ID.
    pub fn cancel(&mut self, scope: &Scope) -> Result<Vec<Event>, Fault> {
        self.scope(scope)?;
        if !matches!(self.phase, Phase::StartingTurn | Phase::Running) {
            return Err(Fault::State);
        }
        self.cancel = true;
        Ok(self.interrupt_event())
    }
    fn interrupt_event(&self) -> Vec<Event> {
        match (&self.thread, &self.active) {
            (Some(thread), Some(turn)) => vec![Event::InterruptRequired {
                thread_id: thread.clone(),
                turn_id: turn.id.clone(),
            }],
            _ => Vec::new(),
        }
    }
    pub fn begin_interrupt(&mut self, scope: &Scope, id: u64) -> Result<Command, Fault> {
        self.scope(scope)?;
        if !matches!(self.phase, Phase::Running | Phase::Failed)
            || self.interrupt_sent
            || self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Interrupt))
        {
            return Err(Fault::State);
        }
        let thread_id = self.thread.clone().ok_or(Fault::State)?;
        let turn_id = self.active_turn_id().ok_or(Fault::State)?.to_owned();
        self.request(id, Pending::Interrupt)?;
        self.cancel = true;
        self.interrupt_sent = true;
        Ok(Command::Interrupt {
            request_id: id,
            thread_id,
            turn_id,
        })
    }
    pub fn begin_unsubscribe(&mut self, scope: &Scope, id: u64) -> Result<Command, Fault> {
        self.scope(scope)?;
        if self.phase != Phase::Ready || !self.pending.is_empty() {
            return Err(Fault::State);
        }
        let thread_id = self.thread.clone().ok_or(Fault::State)?;
        self.request(id, Pending::Unsubscribe)?;
        self.close_seen = false;
        self.unsubscribe_ack = false;
        self.phase = Phase::Closing;
        Ok(Command::Unsubscribe {
            request_id: id,
            thread_id,
        })
    }

    /// `result` is the RPC result only, already correlated by the actor. For
    /// start/resume the actor first validates effective no-executor/config policy.
    pub fn reply(&mut self, scope: &Scope, id: u64, result: &Value) -> Result<Vec<Event>, Fault> {
        self.scope(scope)?;
        let Some(pending) = self.pending.remove(&id) else {
            return Err(Fault::RequestId);
        };
        if !result.is_object() {
            return self.poison(Fault::Protocol);
        }
        let accepted = match pending {
            Pending::Open { resume } => self.opened(result, resume),
            Pending::Turn => self.started(result),
            Pending::Interrupt => {
                if result.as_object().is_some_and(|o| o.is_empty()) {
                    Ok(Vec::new())
                } else {
                    Err(Fault::Protocol)
                }
            }
            Pending::Unsubscribe => {
                if !matches!(
                    result.get("status").and_then(Value::as_str),
                    Some("unsubscribed" | "notSubscribed" | "notLoaded")
                ) {
                    Err(Fault::Protocol)
                } else {
                    self.unsubscribe_ack = true;
                    Ok(self.closed())
                }
            }
        };
        if let Err(fault) = accepted {
            return self.poison(fault);
        }
        accepted
    }
    /// A JSON-RPC error is terminal for this owned attempt, not a resend signal.
    pub fn request_failed(&mut self, scope: &Scope, id: u64) -> Result<(), Fault> {
        self.scope(scope)?;
        if self.pending.remove(&id).is_none() {
            return Err(Fault::RequestId);
        }
        self.poison(Fault::Upstream)
    }

    /// Only for the pinned server's exact same-ID "thread is closing" RPC
    /// rejection after a real closure barrier. The actor verifies that closed
    /// error shape and retains its original absolute open deadline/backoff.
    /// This does not retry a turn, accept an EOF, or invent a closure receipt.
    pub fn resume_deferred(&mut self, scope: &Scope, id: u64) -> Result<(), Fault> {
        self.scope(scope)?;
        if self.phase != Phase::Opening
            || !self.close_seen
            || !self.unsubscribe_ack
            || !matches!(self.pending.get(&id), Some(Pending::Open { resume: true }))
        {
            return Err(Fault::State);
        }
        self.pending.remove(&id);
        if self.resume_attempts >= 3 {
            return self.poison(Fault::Limit);
        }
        self.phase = Phase::Closed;
        Ok(())
    }

    fn opened(&mut self, result: &Value, resume: bool) -> Result<Vec<Event>, Fault> {
        if self.phase != Phase::Opening {
            return Err(Fault::State);
        }
        let thread = result.get("thread").ok_or(Fault::Protocol)?;
        let id = id_field(thread, "id")?;
        if resume && self.thread.as_deref() != Some(id) {
            return Err(Fault::Protocol);
        }
        let choice = self.choice.as_ref().ok_or(Fault::State)?;
        if result.get("model").and_then(Value::as_str) != Some(choice.model())
            || result.get("reasoningEffort").and_then(Value::as_str)
                != Some(choice.effort().as_str())
            || result.get("modelProvider").and_then(Value::as_str) != Some("openai")
            || thread.get("ephemeral").and_then(Value::as_bool) != Some(false)
            || thread.pointer("/status/type").and_then(Value::as_str) != Some("idle")
        {
            return Err(Fault::Protocol);
        }
        self.thread = Some(id.to_owned());
        self.generation = self.generation.checked_add(1).ok_or(Fault::Limit)?;
        self.phase = Phase::Ready;
        self.consumed = false;
        self.close_seen = false;
        self.unsubscribe_ack = false;
        Ok(vec![Event::ThreadReady {
            thread_id: id.to_owned(),
            generation: self.generation,
        }])
    }
    fn started(&mut self, result: &Value) -> Result<Vec<Event>, Fault> {
        if self.phase != Phase::StartingTurn {
            return Err(Fault::State);
        }
        let turn = result.get("turn").ok_or(Fault::Protocol)?;
        let id = id_field(turn, "id")?;
        if turn.get("status").and_then(Value::as_str) != Some("inProgress")
            || !turn.get("error").is_some_and(Value::is_null)
            || !self.seen_turns.insert(id.to_owned())
        {
            return Err(Fault::Protocol);
        }
        if self.seen_turns.len() > 4096 {
            return Err(Fault::Limit);
        }
        self.active = Some(Active {
            id: id.to_owned(),
            items: BTreeMap::new(),
            order: Vec::new(),
            text_bytes: 0,
            usage: None,
        });
        self.phase = Phase::Running;
        let mut events = vec![Event::TurnStarted {
            turn_id: id.to_owned(),
        }];
        let early = std::mem::take(&mut self.early);
        self.early_bytes = 0;
        for (method, params) in early {
            events.extend(self.notification_inner(&method, &params)?);
        }
        if self.cancel && self.phase == Phase::Running {
            events.extend(self.interrupt_event());
        }
        Ok(events)
    }
    fn closed(&mut self) -> Vec<Event> {
        if self.phase == Phase::Closing && self.close_seen && self.unsubscribe_ack {
            self.phase = Phase::Closed;
            self.resume_attempts = 0;
            vec![Event::Closed {
                thread_id: self.thread.clone().expect("closing owns thread"),
                generation: self.generation,
            }]
        } else {
            Vec::new()
        }
    }

    /// Actor passes only protocol notifications here, never server requests.
    /// Wrong native scope fails without mutating the current conversation.
    pub fn notification(
        &mut self,
        scope: &Scope,
        method: &str,
        params: &Value,
    ) -> Result<Vec<Event>, Fault> {
        self.scope(scope)?;
        if self.phase == Phase::Failed {
            return Ok(Vec::new());
        }
        let result = self.notification_inner(method, params);
        if let Err(fault) = result {
            return self.poison(fault);
        }
        result
    }
    fn notification_inner(&mut self, method: &str, params: &Value) -> Result<Vec<Event>, Fault> {
        if method == "thread/closed" {
            if params.get("threadId").and_then(Value::as_str) != self.thread.as_deref() {
                return Ok(Vec::new());
            }
            if self.phase != Phase::Closing {
                return Err(Fault::OutcomeUnknown);
            }
            self.close_seen = true;
            return Ok(self.closed());
        }
        if params.get("threadId").and_then(Value::as_str) != self.thread.as_deref() {
            return Ok(Vec::new());
        }
        if self.phase == Phase::StartingTurn {
            // A core task may publish before the JSON-RPC turn/start response.
            // Buffer bounded *uninterpreted* events; bind to the returned turn ID.
            let size = serde_json::to_vec(params)
                .map_err(|_| Fault::Protocol)?
                .len();
            if self.early.len() >= MAX_EARLY_EVENTS
                || self.early_bytes.saturating_add(size) > MAX_TEXT
            {
                return Err(Fault::Limit);
            }
            self.early_bytes += size;
            self.early.push((method.to_owned(), params.clone()));
            return Ok(Vec::new());
        }
        if self.phase != Phase::Running {
            return Ok(Vec::new());
        }
        let id = if matches!(method, "turn/started" | "turn/completed") {
            params.pointer("/turn/id")
        } else {
            params.get("turnId")
        };
        let turn_scoped = method.starts_with("turn/")
            || method.starts_with("item/")
            || matches!(method, "error" | "thread/tokenUsage/updated");
        let id = id.and_then(Value::as_str);
        if turn_scoped && id.is_none() {
            return Err(Fault::Protocol);
        }
        if id != self.active_turn_id() {
            return Ok(Vec::new());
        }
        self.events += 1;
        if self.events > MAX_EVENTS {
            return Err(Fault::Limit);
        }
        match method {
            "turn/started" => {
                if params.pointer("/turn/status").and_then(Value::as_str) != Some("inProgress") {
                    return Err(Fault::Protocol);
                }
                Ok(Vec::new())
            }
            "item/agentMessage/delta" => self.delta(params),
            "item/started" | "item/completed" => self.item(
                params.get("item").ok_or(Fault::Protocol)?,
                method == "item/completed",
            ),
            "thread/tokenUsage/updated" => {
                let usage = parse_usage(params.get("tokenUsage").ok_or(Fault::Protocol)?)?;
                self.active.as_mut().ok_or(Fault::State)?.usage = Some(usage);
                Ok(Vec::new())
            }
            "turn/completed" => self.completed(params.get("turn").ok_or(Fault::Protocol)?),
            "error" => {
                let will_retry = params
                    .get("willRetry")
                    .and_then(Value::as_bool)
                    .ok_or(Fault::Protocol)?;
                // Validate shape without retaining or exposing provider details.
                let message = params
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .ok_or(Fault::Protocol)?;
                if message.len() > MAX_TEXT {
                    return Err(Fault::Limit);
                }
                Ok(vec![Event::ProviderError { will_retry }])
            }
            // Reasoning content is neither an action nor presentation output.
            "item/reasoning/summaryTextDelta"
            | "item/reasoning/summaryPartAdded"
            | "item/reasoning/textDelta" => Ok(Vec::new()),
            // A matched turn notification not in the closed text vocabulary must
            // not accidentally certify tool execution, approval or delegation.
            _ => Err(Fault::UnsupportedAction),
        }
    }
    fn delta(&mut self, params: &Value) -> Result<Vec<Event>, Fault> {
        let id = id_field(params, "itemId")?;
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .ok_or(Fault::Protocol)?;
        let active = self.active.as_mut().ok_or(Fault::State)?;
        active.text_bytes = active
            .text_bytes
            .checked_add(delta.len())
            .ok_or(Fault::Limit)?;
        if active.text_bytes > MAX_TEXT {
            return Err(Fault::Limit);
        }
        let item = active.ensure_item(id)?;
        if item.completed.is_some() {
            return Err(Fault::Protocol);
        }
        item.delta.push_str(delta);
        if self.cancel || delta.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(vec![Event::Delta {
                item_id: id.to_owned(),
                text: delta.to_owned(),
            }])
        }
    }
    fn item(&mut self, value: &Value, completed: bool) -> Result<Vec<Event>, Fault> {
        match value.get("type").and_then(Value::as_str) {
            Some("agentMessage") => {
                let message = parse_message(value)?;
                let active = self.active.as_mut().ok_or(Fault::State)?;
                let item = active.ensure_item(&message.id)?;
                if !completed {
                    return Ok(Vec::new());
                }
                if let Some(previous) = &item.completed {
                    if previous == &message {
                        return Ok(Vec::new());
                    }
                    return Err(Fault::Protocol);
                }
                // Original final item text is authoritative, even if it revises
                // provisional deltas. No undocumented prefix invariant and no
                // duplicate concatenation are imposed on the original client.
                let extra = message.text.len().saturating_sub(item.delta.len());
                item.completed = Some(message.clone());
                active.text_bytes = active.text_bytes.checked_add(extra).ok_or(Fault::Limit)?;
                if active.text_bytes > MAX_TEXT {
                    return Err(Fault::Limit);
                }
                if self.cancel {
                    Ok(Vec::new())
                } else {
                    Ok(vec![Event::ItemCompleted(message)])
                }
            }
            Some("userMessage") => {
                let content = value
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or(Fault::Protocol)?;
                if content.len() > 256
                    || content
                        .iter()
                        .any(|p| p.get("type").and_then(Value::as_str) != Some("text"))
                {
                    return Err(Fault::UnsupportedAction);
                }
                Ok(Vec::new())
            }
            Some("reasoning" | "contextCompaction") => Ok(Vec::new()),
            _ => Err(Fault::UnsupportedAction),
        }
    }
    fn completed(&mut self, turn: &Value) -> Result<Vec<Event>, Fault> {
        let outcome = match turn.get("status").and_then(Value::as_str) {
            Some("completed") if turn.get("error").is_some_and(Value::is_null) => {
                Outcome::Completed
            }
            Some("interrupted") => Outcome::Interrupted,
            Some("failed") => Outcome::Failed,
            _ => return Err(Fault::Protocol),
        };
        let items = turn
            .get("items")
            .and_then(Value::as_array)
            .ok_or(Fault::Protocol)?;
        if items.len() > MAX_ITEMS
            || !matches!(
                turn.get("itemsView").and_then(Value::as_str),
                Some("summary" | "notLoaded" | "full")
            )
        {
            return Err(Fault::Protocol);
        }
        let mut events = Vec::new();
        for item in items {
            events.extend(self.item(item, true)?);
        }
        let active = self.active.take().ok_or(Fault::State)?;
        // Summary/notLoaded is not a promise of a full item ledger. As in the
        // original collector, only observed completed items become final text;
        // a delta-only item is never promoted to a fabricated final message.
        let result = active.finish(
            self.thread.clone().ok_or(Fault::State)?,
            outcome,
            self.cancel,
        );
        self.phase = Phase::Ready;
        events.push(Event::Terminal(result));
        Ok(events)
    }
    /// Every server request is unsupported in this text-only connection. The
    /// actor rejects it and interrupts/terminates; it must never send success.
    pub fn server_request(&mut self, scope: &Scope) -> Result<(), Fault> {
        self.scope(scope)?;
        self.poison(Fault::UnsupportedAction)
    }
    /// EOF/cancelled transport cannot manufacture a successful terminal or
    /// authorize a resend. The actor must preserve this uncertain disposition.
    pub fn transport_lost(&mut self, scope: &Scope) -> Result<Vec<Event>, Fault> {
        self.scope(scope)?;
        self.phase = Phase::Failed;
        self.pending.clear();
        self.early.clear();
        let mut events = vec![Event::TransportLost {
            thread_id: self.thread.clone(),
            turn_id: self.active_turn_id().map(str::to_owned),
        }];
        if let Some(active) = self.active.take() {
            events.push(Event::Terminal(active.finish(
                self.thread.clone().ok_or(Fault::State)?,
                Outcome::Unknown,
                self.cancel,
            )));
        }
        Ok(events)
    }
}

impl Active {
    fn ensure_item(&mut self, id: &str) -> Result<&mut Item, Fault> {
        if !self.items.contains_key(id) {
            if self.items.len() >= MAX_ITEMS {
                return Err(Fault::Limit);
            }
            self.order.push(id.to_owned());
            self.items.insert(
                id.to_owned(),
                Item {
                    delta: String::new(),
                    completed: None,
                },
            );
        }
        self.items.get_mut(id).ok_or(Fault::State)
    }
    fn finish(mut self, thread_id: String, outcome: Outcome, cancel_requested: bool) -> TurnResult {
        let messages = self
            .order
            .into_iter()
            .filter_map(|id| self.items.remove(&id)?.completed)
            .collect();
        TurnResult {
            thread_id,
            turn_id: self.id,
            outcome,
            cancel_requested,
            messages,
            usage: self.usage,
        }
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn id_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, Fault> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| identifier(s))
        .ok_or(Fault::Protocol)
}
fn parse_message(value: &Value) -> Result<Message, Fault> {
    let id = id_field(value, "id")?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or(Fault::Protocol)?;
    if text.len() > MAX_TEXT {
        return Err(Fault::Limit);
    }
    let phase = match value.get("phase") {
        Some(Value::Null) | None => MessagePhase::Unknown,
        Some(Value::String(s)) if s == "commentary" => MessagePhase::Commentary,
        Some(Value::String(s)) if s == "partial_answer" => MessagePhase::PartialAnswer,
        Some(Value::String(s)) if s == "final_answer" => MessagePhase::FinalAnswer,
        _ => return Err(Fault::Protocol),
    };
    if value.get("delivery").is_some_and(|v| !v.is_null())
        || value
            .get("questions")
            .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Fault::UnsupportedAction);
    }
    Ok(Message {
        id: id.to_owned(),
        text: text.to_owned(),
        phase,
    })
}
fn parse_usage(value: &Value) -> Result<UsageSnapshot, Fault> {
    let total = usage(value.get("total").ok_or(Fault::Protocol)?)?;
    let last = usage(value.get("last").ok_or(Fault::Protocol)?)?;
    let model_context_window = match value.get("modelContextWindow") {
        Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or(Fault::Protocol)?),
        None => return Err(Fault::Protocol),
    };
    Ok(UsageSnapshot {
        total,
        last,
        model_context_window,
    })
}
fn usage(value: &Value) -> Result<Usage, Fault> {
    let count = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or(Fault::Protocol)
    };
    let usage = Usage {
        total_tokens: count("totalTokens")?,
        input_tokens: count("inputTokens")?,
        cached_input_tokens: count("cachedInputTokens")?,
        cache_write_input_tokens: count("cacheWriteInputTokens")?,
        output_tokens: count("outputTokens")?,
        reasoning_output_tokens: count("reasoningOutputTokens")?,
    };
    if usage.cached_input_tokens > usage.input_tokens
        || usage.reasoning_output_tokens > usage.output_tokens
    {
        return Err(Fault::Protocol);
    }
    Ok(usage)
}

#[cfg(test)]
#[path = "conversation/tests.rs"]
mod tests;
