//! Move-only original turn intent and exact pre-write evidence. These types prove
//! no model-frame handoff, not thread/process settlement or durable Store commit.
use super::write_fence::WriteFence;
use crate::{browser_port::BrowserPolicy, catalog::EffortSelection, conversation::Scope};
#[path = "write_receipt/browser.rs"]
mod browser;
#[cfg(windows)]
pub(crate) use browser::BrowserInputIntent;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{io::Write, sync::Arc, time::Instant};

/// Supplied only after the serialized native owner validates this exact history.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TurnHistory {
    pub(crate) incarnation: [u8; 16],
    pub(crate) grant_epoch: u64,
    pub(crate) conversation: String,
    pub(crate) reply: u64,
    pub(crate) previous_reply: Option<u64>,
    pub(crate) expected_revision: Option<u64>,
    pub(crate) thread_id: Option<String>,
}
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TurnOperation {
    scope: Scope,
    history: TurnHistory,
    selection: EffortSelection,
    draft_digest: [u8; 32],
    model: String,
    input: InputBinding,
}
#[derive(Clone, Eq, PartialEq)]
enum InputBinding {
    TextOnly,
    BrowserPending(BrowserPolicy),
    BrowserPrepared {
        policy: BrowserPolicy,
        digest: [u8; 32],
    },
}
impl TurnOperation {
    pub(crate) fn new(
        scope: Scope,
        history: TurnHistory,
        model: &str,
        selection: EffortSelection,
        text: &str,
    ) -> Option<Self> {
        Self::with_limit(scope, history, model, selection, text, 32 * 1024)
    }
    fn with_limit(
        scope: Scope,
        history: TurnHistory,
        model: &str,
        selection: EffortSelection,
        text: &str,
        maximum: usize,
    ) -> Option<Self> {
        if history.incarnation == [0; 16]
            || history.grant_epoch == 0
            || history.reply == 0
            || history.conversation.is_empty()
            || history.conversation.len() > 40
            || !history
                .conversation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || history.expected_revision == Some(0)
            || history
                .previous_reply
                .is_some_and(|reply| reply == 0 || reply >= history.reply)
            || (history.expected_revision.is_none()
                && (history.previous_reply.is_some() || history.thread_id.is_some()))
            || history
                .thread_id
                .as_deref()
                .is_some_and(|thread| !identifier(thread))
            || !identifier(model)
            || text.len() > maximum
        {
            return None;
        }
        Some(Self {
            scope,
            history,
            selection,
            draft_digest: draft_digest(model, selection, text),
            model: model.into(),
            input: InputBinding::TextOnly,
        })
    }
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn draft_digest(model: &str, selection: EffortSelection, text: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"codex-turn-draft-v2");
    match selection {
        EffortSelection::Default => hash.update([0]),
        EffortSelection::Explicit(effort) => {
            hash.update([1]);
            hash.update((effort.as_str().len() as u64).to_le_bytes());
            hash.update(effort.as_str().as_bytes());
        }
    }
    for value in [model, text] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize().into()
}
#[derive(Clone)]
pub(crate) struct SessionStamp(Arc<()>);
impl SessionStamp {
    #[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
    pub(super) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(super) fn new() -> Self {
        Self(Arc::new(()))
    }
}

/// Allocated once by Session for a current native operation. There is no Clone,
/// Deserialize, raw-field constructor or renderer-visible representation.
pub(crate) struct TurnIntent(Box<IntentInner>);
struct IntentInner {
    stamp: SessionStamp,
    sequence: u64,
    operation: TurnOperation,
}
impl TurnIntent {
    pub(super) fn new(stamp: SessionStamp, sequence: u64, operation: TurnOperation) -> Self {
        Self(Box::new(IntentInner {
            stamp,
            sequence,
            operation,
        }))
    }
    /// Binding the real RPC ID and fixed bytes consumes local no-handoff authority.
    /// A preparation error returns the original still-unhanded intent.
    pub(crate) fn into_write(
        self,
        wire_id: u64,
        thread_id: &str,
        message: Value,
    ) -> Result<TurnWrite, Self> {
        if !valid_turn_frame(&self.0.operation, wire_id, thread_id, &message) {
            return Err(self);
        }
        let Ok(bytes) = serde_json::to_vec(&message) else {
            return Err(self);
        };
        let inner = *self.0;
        let digest = frame_digest(&inner.operation, &bytes);
        let binding = TurnBinding {
            operation: inner.operation,
            wire_id,
            frame_digest: digest,
        };
        Ok(TurnWrite {
            ticket: TurnTicket {
                stamp: inner.stamp,
                sequence: inner.sequence,
                binding,
            },
            bytes,
        })
    }
    pub(super) fn reject(
        self,
        session: &SessionStamp,
        maximum_sequence: u64,
    ) -> Option<UnsentTurn> {
        (self.0.sequence != 0
            && self.0.sequence <= maximum_sequence
            && Arc::ptr_eq(&self.0.stamp.0, &session.0))
        .then_some(UnsentTurn(Box::new(UnsentInner {
            operation: self.0.operation,
            wire_id: None,
        })))
    }
}
fn valid_turn_frame(
    operation: &TurnOperation,
    wire_id: u64,
    thread: &str,
    message: &Value,
) -> bool {
    let Some(envelope) = message.as_object() else {
        return false;
    };
    let Some(params) = message.get("params").and_then(Value::as_object) else {
        return false;
    };
    let Some(input) = params.get("input").and_then(Value::as_array) else {
        return false;
    };
    let Some(item) = input.first().and_then(Value::as_object) else {
        return false;
    };
    let (Some(model), Some(text)) = (
        params.get("model").and_then(Value::as_str),
        item.get("text").and_then(Value::as_str),
    ) else {
        return false;
    };
    wire_id != 0
        && identifier(thread)
        && envelope.len() == 3
        && message.get("id").and_then(Value::as_u64) == Some(wire_id)
        && message.get("method").and_then(Value::as_str) == Some("turn/start")
        && match operation.selection {
            EffortSelection::Default => params.len() == 4 && !params.contains_key("effort"),
            EffortSelection::Explicit(effort) => {
                params.len() == 5
                    && params.get("effort").and_then(Value::as_str) == Some(effort.as_str())
            }
        }
        && params.get("threadId").and_then(Value::as_str) == Some(thread)
        && operation
            .history
            .thread_id
            .as_deref()
            .is_none_or(|known| known == thread)
        && params
            .get("environments")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        && input.len() == 1
        && item.len() == 3
        && item.get("type").and_then(Value::as_str) == Some("text")
        && item
            .get("text_elements")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        && operation.model == model
        && match operation.input {
            InputBinding::TextOnly => {
                operation.draft_digest == draft_digest(model, operation.selection, text)
            }
            InputBinding::BrowserPending(_) => false,
            InputBinding::BrowserPrepared { digest, .. } => {
                digest == browser::prepared_digest(text)
            }
        }
}
fn frame_digest(operation: &TurnOperation, bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"codex-original-turn-frame-v3");
    hash.update(operation.scope.correlation_digest());
    hash.update(operation.history.incarnation);
    hash.update(operation.history.grant_epoch.to_le_bytes());
    hash.update(operation.history.reply.to_le_bytes());
    for number in [
        operation.history.previous_reply,
        operation.history.expected_revision,
    ] {
        hash.update([u8::from(number.is_some())]);
        hash.update(number.unwrap_or(0).to_le_bytes());
    }
    for value in [
        operation.history.conversation.as_str(),
        operation.history.thread_id.as_deref().unwrap_or(""),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    hash.update(operation.draft_digest);
    match operation.input {
        InputBinding::TextOnly => hash.update([0]),
        InputBinding::BrowserPending(policy) => {
            hash.update([1]);
            hash.update(policy.tool_digest());
            hash.update(policy.instruction_digest());
        }
        InputBinding::BrowserPrepared { policy, digest } => {
            hash.update([2]);
            hash.update(policy.tool_digest());
            hash.update(policy.instruction_digest());
            hash.update(digest);
        }
    }
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
    hash.finalize().into()
}
#[derive(Clone, Eq, PartialEq)]
pub(super) struct TurnBinding {
    pub(super) operation: TurnOperation,
    pub(super) wire_id: u64,
    frame_digest: [u8; 32],
}
pub(super) struct TurnTicket {
    stamp: SessionStamp,
    pub(super) sequence: u64,
    pub(super) binding: TurnBinding,
}
impl TurnTicket {
    pub(super) fn belongs_to(&self, stamp: &SessionStamp) -> bool {
        Arc::ptr_eq(&self.stamp.0, &stamp.0)
    }
    pub(super) fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.binding.frame_digest == frame_digest(&self.binding.operation, bytes)
    }
}
pub(crate) struct TurnWrite {
    ticket: TurnTicket,
    bytes: Vec<u8>,
}
impl TurnWrite {
    pub(super) fn into_parts(mut self) -> (TurnTicket, Vec<u8>) {
        // Replace only owned metadata to permit zeroizing Drop; this does not
        // mint another intent, ticket constructor or live Session stamp.
        let ticket = TurnTicket {
            stamp: self.ticket.stamp.clone(),
            sequence: self.ticket.sequence,
            binding: self.ticket.binding.clone(),
        };
        (ticket, std::mem::take(&mut self.bytes))
    }
}
impl Drop for TurnWrite {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

enum Rejection {
    Unqueued,
    Expired,
    Retired,
}
pub(super) struct NeverStarted(Box<RejectedInner>);
struct RejectedInner {
    stamp: SessionStamp,
    sequence: u64,
    binding: Option<TurnBinding>,
    reason: Rejection,
}
impl NeverStarted {
    pub(super) fn expired(&self) -> bool {
        matches!(self.0.reason, Rejection::Expired)
    }
    pub(super) fn consume(
        self,
        stamp: &SessionStamp,
        sequence: u64,
        expected: &TurnBinding,
    ) -> Option<UnsentTurn> {
        (Arc::ptr_eq(&self.0.stamp.0, &stamp.0)
            && self.0.sequence == sequence
            && self.0.binding.as_ref() == Some(expected))
        .then_some(UnsentTurn(Box::new(UnsentInner {
            operation: expected.operation.clone(),
            wire_id: Some(expected.wire_id),
        })))
    }
}
/// Move-only scoped non-dispatch evidence, not a generic clean/settled token.
pub(crate) struct UnsentTurn(Box<UnsentInner>);
struct UnsentInner {
    operation: TurnOperation,
    wire_id: Option<u64>,
}
impl UnsentTurn {
    pub(crate) fn wire_id(&self) -> Option<u64> {
        self.0.wire_id
    }
    pub(crate) fn consume(self, expected: &TurnOperation) -> Option<MatchedUnsent> {
        (self.0.operation == *expected).then_some(MatchedUnsent {
            operation: self.0.operation,
        })
    }
}
/// Non-Clone matched proof retained until the separate cleanup and Store Skip
/// barriers complete. A cloned TurnOperation alone cannot create this value.
pub(crate) struct MatchedUnsent {
    operation: TurnOperation,
}
impl MatchedUnsent {
    pub(crate) fn is_for(&self, operation: &TurnOperation) -> bool {
        self.operation == *operation
    }
}

pub(super) enum WriteOutcome {
    Written,
    Rejected(NeverStarted),
    EnteredFailure,
}
pub(super) struct OwnedFrame {
    stamp: SessionStamp,
    sequence: u64,
    binding: Option<TurnBinding>,
    bytes: Vec<u8>,
    deadline: Instant,
    fence: Option<WriteFence>,
}
impl OwnedFrame {
    pub(super) fn new(
        stamp: SessionStamp,
        sequence: u64,
        binding: Option<TurnBinding>,
        bytes: Vec<u8>,
        deadline: Instant,
        fence: Option<WriteFence>,
    ) -> Self {
        Self {
            stamp,
            sequence,
            binding,
            bytes,
            deadline,
            fence,
        }
    }
    pub(super) fn reject_unqueued(self) -> NeverStarted {
        self.reject(Rejection::Unqueued)
    }
    fn reject(mut self, reason: Rejection) -> NeverStarted {
        let receipt = NeverStarted(Box::new(RejectedInner {
            stamp: self.stamp.clone(),
            sequence: self.sequence,
            binding: self.binding.take(),
            reason,
        }));
        self.bytes.fill(0);
        receipt
    }
    pub(super) fn write(mut self, writer: &mut impl Write) -> WriteOutcome {
        if Instant::now() >= self.deadline {
            return WriteOutcome::Rejected(self.reject(Rejection::Expired));
        }
        if self.fence.as_ref().is_some_and(|fence| !fence.current()) {
            return WriteOutcome::Rejected(self.reject(Rejection::Retired));
        }
        let result = writer.write_all(&self.bytes).and_then(|()| writer.flush());
        self.bytes.fill(0);
        if result.is_ok() {
            WriteOutcome::Written
        } else {
            WriteOutcome::EnteredFailure
        }
    }
}
impl Drop for OwnedFrame {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
        time::Duration,
    };
    fn operation() -> TurnOperation {
        TurnOperation::new(
            Scope::new("profile".into(), [1; 16], 1).unwrap(),
            TurnHistory {
                incarnation: [2; 16],
                grant_epoch: 3,
                conversation: "page-1".into(),
                reply: 5,
                previous_reply: Some(3),
                expected_revision: Some(7),
                thread_id: Some("thread-1".into()),
            },
            "model",
            EffortSelection::Explicit(crate::catalog::Effort::High),
            "hello",
        )
        .unwrap()
    }
    fn message() -> Value {
        json!({"id":9,"method":"turn/start","params":{"threadId":"thread-1","model":"model","effort":"high","input":[{"type":"text","text":"hello","text_elements":[]}],"environments":[]}})
    }
    fn prepared(stamp: &SessionStamp, op: &TurnOperation) -> TurnWrite {
        TurnIntent::new(stamp.clone(), 1, op.clone())
            .into_write(9, "thread-1", message())
            .ok()
            .unwrap()
    }
    fn frame(
        stamp: &SessionStamp,
        op: &TurnOperation,
        fence: Option<WriteFence>,
        deadline: Instant,
    ) -> (TurnBinding, OwnedFrame) {
        let (ticket, mut bytes) = prepared(stamp, op).into_parts();
        assert!(ticket.belongs_to(stamp) && ticket.matches_bytes(&bytes));
        let binding = ticket.binding;
        bytes.push(b'\n');
        (
            binding.clone(),
            OwnedFrame::new(stamp.clone(), 1, Some(binding), bytes, deadline, fence),
        )
    }
    #[test]
    fn queued_browser_write_rechecks_same_live_budget_after_wait_ends() {
        struct Budget(std::sync::Mutex<Instant>);
        impl crate::browser_port::BrowserBudget for Budget {
            fn deadline(&self) -> Result<Instant, crate::browser_port::BrowserPortFault> {
                Ok(*self.0.lock().unwrap())
            }
            fn hard_deadline(&self) -> Result<Instant, crate::browser_port::BrowserPortFault> {
                self.deadline()
            }
        }
        let budget = Arc::new(Budget(std::sync::Mutex::new(
            Instant::now() + Duration::from_secs(300),
        )));
        let fence = WriteFence::text(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(1)),
            1,
        )
        .with_browser_budget(budget.clone())
        .unwrap();
        let stamp = SessionStamp::new();
        let op = operation();
        let (_, queued) = frame(
            &stamp,
            &op,
            Some(fence),
            Instant::now() + Duration::from_secs(390),
        );
        *budget.0.lock().unwrap() = Instant::now() - Duration::from_millis(1);
        let mut bytes = Vec::new();
        assert!(matches!(
            queued.write(&mut bytes),
            WriteOutcome::Rejected(_)
        ));
        assert!(bytes.is_empty());
    }
    #[test]
    fn intent_is_same_session_and_operation_only_without_a_fake_rpc_id() {
        let stamp = SessionStamp::new();
        let op = operation();
        let receipt = TurnIntent::new(stamp.clone(), 1, op.clone())
            .reject(&stamp, 1)
            .unwrap();
        assert_eq!(receipt.wire_id(), None);
        assert!(receipt.consume(&op).unwrap().is_for(&op));
        assert!(TurnIntent::new(stamp, 1, op)
            .reject(&SessionStamp::new(), 1)
            .is_none());
    }
    #[test]
    fn fixed_payload_and_real_rpc_binding_cannot_change_draft_or_owned_thread() {
        let stamp = SessionStamp::new();
        let op = operation();
        for changed in 0..6 {
            let mut value = message();
            match changed {
                0 => value["id"] = json!(10),
                1 => value["params"]["input"][0]["text"] = json!("other"),
                2 => value["params"]["model"] = json!("other"),
                3 => value["params"]["effort"] = json!("low"),
                4 => value["params"]["threadId"] = json!("other"),
                _ => value["params"]["extra"] = json!(true),
            }
            let intent = TurnIntent::new(stamp.clone(), 1, op.clone())
                .into_write(9, "thread-1", value)
                .err()
                .unwrap();
            assert!(intent.reject(&stamp, 1).unwrap().consume(&op).is_some());
        }
        let (ticket, mut bytes) = prepared(&stamp, &op).into_parts();
        assert!(ticket.matches_bytes(&bytes));
        bytes.push(b' ');
        assert!(!ticket.matches_bytes(&bytes));
    }
    #[test]
    fn omitted_default_and_explicit_none_are_distinct_operations_and_fixed_frames() {
        use crate::catalog::Effort;
        let base = operation();
        let default = TurnOperation::new(
            base.scope.clone(),
            base.history.clone(),
            "model",
            EffortSelection::Default,
            "hello",
        )
        .unwrap();
        let none = TurnOperation::new(
            base.scope.clone(),
            base.history.clone(),
            "model",
            EffortSelection::Explicit(Effort::None),
            "hello",
        )
        .unwrap();
        assert!(default.draft_digest != none.draft_digest);
        let mut omitted = message();
        omitted["params"].as_object_mut().unwrap().remove("effort");
        let mut literal = message();
        literal["params"]["effort"] = json!("none");
        let mut null = message();
        null["params"]["effort"] = Value::Null;
        for (op, valid, invalid) in [
            (&default, omitted.clone(), literal.clone()),
            (&none, literal, omitted),
        ] {
            let stamp = SessionStamp::new();
            assert!(TurnIntent::new(stamp.clone(), 1, op.clone())
                .into_write(9, "thread-1", valid)
                .is_ok());
            for value in [invalid, null.clone()] {
                let intent = TurnIntent::new(stamp.clone(), 2, op.clone())
                    .into_write(9, "thread-1", value)
                    .err()
                    .unwrap();
                assert!(intent.reject(&stamp, 2).unwrap().consume(op).is_some());
            }
        }
        let stamp = SessionStamp::new();
        assert!(TurnIntent::new(stamp.clone(), 1, default)
            .reject(&stamp, 1)
            .unwrap()
            .consume(&none)
            .is_none());
    }
    #[test]
    fn preparation_fence_is_scope_only_and_never_a_model_write_fence() {
        let revision = Arc::new(AtomicU64::new(4));
        let prepare = WriteFence::preparation(revision.clone(), 4);
        assert!(prepare.current());
        assert!(!prepare.is_text());
        let text = WriteFence::text(Arc::new(AtomicBool::new(true)), revision.clone(), 4);
        assert!(text.is_text());
        assert!(!text.current());
        assert!(!WriteFence::login(Arc::new(AtomicBool::new(false))).is_text());
        revision.store(5, Ordering::Release);
        assert!(!prepare.current());
    }

    #[test]
    fn queued_cancel_revision_and_expiry_produce_exact_zero_byte_receipt() {
        for reason in 0..3 {
            let stamp = SessionStamp::new();
            let op = operation();
            let cancel = Arc::new(AtomicBool::new(false));
            let revision = Arc::new(AtomicU64::new(4));
            let fence = WriteFence::text(cancel.clone(), revision.clone(), 4);
            let deadline = if reason == 2 {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(1)
            };
            let (binding, frame) = frame(&stamp, &op, Some(fence), deadline);
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            tx.send(frame).ok().unwrap();
            if reason == 0 {
                cancel.store(true, Ordering::Release);
            }
            if reason == 1 {
                revision.store(5, Ordering::Release);
            }
            let mut bytes = Vec::new();
            let WriteOutcome::Rejected(receipt) = rx.recv().unwrap().write(&mut bytes) else {
                panic!("exact gate rejection required")
            };
            assert!(bytes.is_empty());
            assert_eq!(receipt.expired(), reason == 2);
            let unsent = receipt.consume(&stamp, 1, &binding).unwrap();
            assert_eq!(unsent.wire_id(), Some(9));
            assert!(unsent.consume(&op).is_some());
        }
    }
    #[test]
    fn receipts_reject_other_session_attempt_history_and_operation() {
        let stamp = SessionStamp::new();
        let op = operation();
        let (binding, frame) = frame(&stamp, &op, None, Instant::now());
        assert!(frame
            .reject_unqueued()
            .consume(&SessionStamp::new(), 1, &binding)
            .is_none());
        let (binding, frame) = self::frame(&stamp, &op, None, Instant::now());
        assert!(frame
            .reject_unqueued()
            .consume(&stamp, 2, &binding)
            .is_none());
        let mut variants = Vec::new();
        let mut other = op.clone();
        other.history.incarnation = [3; 16];
        variants.push(other);
        let mut other = op.clone();
        other.history.grant_epoch += 1;
        variants.push(other);
        let mut other = op.clone();
        other.history.reply += 1;
        variants.push(other);
        let mut other = op.clone();
        other.history.previous_reply = Some(2);
        variants.push(other);
        let mut other = op.clone();
        other.history.expected_revision = Some(6);
        variants.push(other);
        let mut other = op.clone();
        other.history.expected_revision = None;
        variants.push(other);
        let mut other = op.clone();
        other.history.conversation = "page-2".into();
        variants.push(other);
        let mut other = op.clone();
        other.scope = Scope::new("profile".into(), [3; 16], 1).unwrap();
        variants.push(other);
        let mut other = op.clone();
        other.draft_digest = [4; 32];
        variants.push(other);
        for other in variants {
            assert!(TurnIntent::new(stamp.clone(), 1, op.clone())
                .reject(&stamp, 1)
                .unwrap()
                .consume(&other)
                .is_none());
        }
    }
    struct Failing {
        entered: bool,
        bytes: usize,
        partial: bool,
    }
    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered = true;
            if self.partial && self.bytes == 0 {
                self.bytes = 1;
                return Ok(bytes.len().min(1));
            }
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "fixed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn entered_zero_or_partial_error_and_lost_ack_never_prove_unsent() {
        for partial in [false, true] {
            let mut writer = Failing {
                entered: false,
                bytes: 0,
                partial,
            };
            let (_, frame) = frame(
                &SessionStamp::new(),
                &operation(),
                None,
                Instant::now() + Duration::from_secs(1),
            );
            assert!(matches!(
                frame.write(&mut writer),
                WriteOutcome::EnteredFailure
            ));
            assert!(writer.entered);
        }
        let (tx, rx) = std::sync::mpsc::sync_channel::<WriteOutcome>(1);
        drop(tx);
        assert!(rx.try_recv().is_err());
    }
    struct CancelInsideWrite {
        cancel: Arc<AtomicBool>,
        bytes: Vec<u8>,
    }
    impl Write for CancelInsideWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.cancel.store(true, Ordering::Release);
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn cancellation_after_entered_write_does_not_undo_the_dispatch() {
        let cancel = Arc::new(AtomicBool::new(false));
        let fence = WriteFence::login(cancel.clone());
        let mut writer = CancelInsideWrite {
            cancel,
            bytes: Vec::new(),
        };
        let (_, frame) = frame(
            &SessionStamp::new(),
            &operation(),
            Some(fence),
            Instant::now() + Duration::from_secs(1),
        );
        assert!(matches!(frame.write(&mut writer), WriteOutcome::Written));
        assert!(!writer.bytes.is_empty());
        assert!(writer.cancel.load(Ordering::Acquire));
    }
    #[test]
    fn returned_full_queue_frame_is_still_locally_owned_and_unsent() {
        let stamp = SessionStamp::new();
        let op = operation();
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        let (_, first) = frame(&stamp, &op, None, Instant::now());
        tx.send(first).ok().unwrap();
        let (binding, second) = frame(&stamp, &op, None, Instant::now());
        let returned = match tx.try_send(second).err().unwrap() {
            std::sync::mpsc::TrySendError::Full(v)
            | std::sync::mpsc::TrySendError::Disconnected(v) => v,
        };
        assert!(returned
            .reject_unqueued()
            .consume(&stamp, 1, &binding)
            .unwrap()
            .consume(&op)
            .is_some());
    }
}
