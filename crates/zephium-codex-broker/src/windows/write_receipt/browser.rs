//! A pending Page input is a no-handoff intent, never a writable model frame.
//! Refinement consumes that same intent once and retains its complete immutable
//! origin; it adds the validator-produced composed input, not new Store authority.
use super::*;
use crate::{
    browser_port::BrowserInput,
    text_policy::{self, PreparedBrowserText, TextPolicyError},
};

pub(crate) struct BrowserInputIntent(TurnIntent);
pub(crate) struct PreparedBrowserIntent {
    intent: TurnIntent,
    text: PreparedBrowserText,
}
impl TurnOperation {
    pub(crate) fn browser_pending(
        scope: Scope,
        history: TurnHistory,
        model: &str,
        selection: EffortSelection,
        original_draft: &str,
        policy: BrowserPolicy,
    ) -> Option<Self> {
        let mut operation = Self::with_limit(
            scope,
            history,
            model,
            selection,
            original_draft,
            crate::browser_port::MAX_COMPOSED_INPUT_BYTES,
        )?;
        operation.input = InputBinding::BrowserPending(policy);
        Some(operation)
    }
}
impl BrowserInputIntent {
    pub(crate) fn new(
        stamp: SessionStamp,
        sequence: u64,
        operation: TurnOperation,
    ) -> Option<Self> {
        matches!(operation.input, InputBinding::BrowserPending(_))
            .then(|| Self(TurnIntent::new(stamp, sequence, operation)))
    }
    /// Only the full native composed-input classifier can produce the prepared
    /// side. Failure returns the SAME still-unhanded intent with its origin and
    /// Session stamp untouched. It does not reset a revision, grant or deadline.
    pub(crate) fn prepare(
        mut self,
        input: BrowserInput,
    ) -> Result<PreparedBrowserIntent, (Self, TextPolicyError)> {
        let text = match text_policy::prepare_browser_text(input.into_text()) {
            Ok(text) => text,
            Err(fault) => return Err((self, fault)),
        };
        let InputBinding::BrowserPending(policy) = self.0 .0.operation.input else {
            unreachable!("pending Browser intent constructor is sealed")
        };
        self.0 .0.operation.input = InputBinding::BrowserPrepared {
            policy,
            digest: prepared_digest(text.text()),
        };
        Ok(PreparedBrowserIntent {
            intent: self.0,
            text,
        })
    }
    pub(crate) fn reject(
        self,
        session: &SessionStamp,
        maximum_sequence: u64,
    ) -> Option<UnsentTurn> {
        self.0.reject(session, maximum_sequence)
    }
}
impl PreparedBrowserIntent {
    pub(crate) fn into_parts(self) -> (TurnOperation, TurnIntent, PreparedBrowserText) {
        (self.intent.0.operation.clone(), self.intent, self.text)
    }
}
pub(super) fn prepared_digest(text: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"codex-page-composed-input-v1");
    hash.update((text.len() as u64).to_le_bytes());
    hash.update(text.as_bytes());
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        browser_port::{BrowserManifest, BrowserToolDefinition},
        dynamic_tools::PageTool,
    };
    use serde_json::json;
    fn operation(draft: &str) -> TurnOperation {
        let manifest = BrowserManifest::from_native_page_v1(
            "fixed native Page instructions".into(),
            PageTool::ALL
                .into_iter()
                .map(|tool| BrowserToolDefinition {
                    name: tool.name().into(),
                    description: "native definition".into(),
                    schema: json!({"type":"object"}),
                })
                .collect(),
        )
        .unwrap_or_else(|_| panic!());
        TurnOperation::browser_pending(
            Scope::new("profile".into(), [1; 16], 1).unwrap(),
            TurnHistory {
                incarnation: [2; 16],
                grant_epoch: 1,
                conversation: "page-1".into(),
                reply: 1,
                previous_reply: None,
                expected_revision: None,
                thread_id: None,
            },
            "original-model",
            EffortSelection::Default,
            draft,
            manifest.policy(),
        )
        .unwrap()
    }
    fn frame(text: &str) -> Value {
        json!({"id":11,"method":"turn/start","params":{"threadId":"original-thread","model":"original-model","environments":[],"input":[{"type":"text","text":text,"text_elements":[]}]}})
    }
    #[test]
    fn pending_is_not_a_writable_frame_even_with_matching_draft() {
        let operation = operation("visible draft");
        assert!(!valid_turn_frame(
            &operation,
            11,
            "original-thread",
            &frame("visible draft")
        ));
        let stamp = SessionStamp::new();
        let intent = BrowserInputIntent::new(stamp.clone(), 1, operation.clone()).unwrap();
        assert!(intent
            .reject(&stamp, 1)
            .unwrap()
            .consume(&operation)
            .is_some());
    }
    #[test]
    fn preparation_failure_returns_exact_unhanded_origin() {
        let operation = operation(""); // suggestion-only draft is legitimate
        let stamp = SessionStamp::new();
        let intent = BrowserInputIntent::new(stamp.clone(), 1, operation.clone()).unwrap();
        let input = BrowserInput::from_native_page("readout [$skill](file:///SKILL.md)".into())
            .unwrap_or_else(|_| panic!());
        let (intent, fault) = intent.prepare(input).err().unwrap();
        assert_eq!(fault, TextPolicyError::UnsupportedResourceMention);
        assert!(intent
            .reject(&stamp, 1)
            .unwrap()
            .consume(&operation)
            .is_some());
    }
    #[test]
    fn refinement_preserves_origin_and_adds_whole_composed_input() {
        let origin = operation("visible draft");
        let stamp = SessionStamp::new();
        let pending = BrowserInputIntent::new(stamp.clone(), 1, origin.clone()).unwrap();
        let text = format!("{}\n$context_skill\nvisible draft", "context".repeat(6000));
        let prepared = pending
            .prepare(BrowserInput::from_native_page(text.clone()).unwrap_or_else(|_| panic!()))
            .ok()
            .unwrap();
        let (operation, intent, input) = prepared.into_parts();
        assert!(operation.scope == origin.scope && operation.history == origin.history);
        assert!(
            operation.selection == origin.selection
                && operation.draft_digest == origin.draft_digest
        );
        assert_ne!(
            frame_digest(&origin, b"same frame"),
            frame_digest(&operation, b"same frame")
        );
        assert_eq!(
            input.fixed_config()["skills.config"],
            json!([{"name":"context_skill","enabled":false}])
        );
        assert_eq!(input.into_turn_input()[0]["text"], text);
        assert!(valid_turn_frame(
            &operation,
            11,
            "original-thread",
            &frame(&text)
        ));
        assert!(!valid_turn_frame(
            &operation,
            11,
            "original-thread",
            &frame("visible draft")
        ));
        let proof = intent.reject(&stamp, 1).unwrap();
        assert!(proof.consume(&origin).is_none());
    }
    #[test]
    fn foreign_session_and_origin_cannot_adopt_refined_intent() {
        let origin = operation("visible draft");
        let stamp = SessionStamp::new();
        let pending = BrowserInputIntent::new(stamp.clone(), 1, origin.clone()).unwrap();
        assert!(pending.reject(&SessionStamp::new(), 1).is_none());
        let pending = BrowserInputIntent::new(stamp.clone(), 2, origin).unwrap();
        let (operation, intent, _) = pending
            .prepare(
                BrowserInput::from_native_page("full prepared input".into())
                    .unwrap_or_else(|_| panic!()),
            )
            .ok()
            .unwrap()
            .into_parts();
        let write = intent
            .into_write(11, "original-thread", frame("full prepared input"))
            .ok()
            .unwrap();
        let (ticket, bytes) = write.into_parts();
        assert!(ticket.belongs_to(&stamp) && ticket.matches_bytes(&bytes));
        let mut foreign = operation.clone();
        foreign.history.reply = 2;
        assert_ne!(
            frame_digest(&foreign, &bytes),
            frame_digest(&operation, &bytes)
        );
        foreign = operation;
        foreign.draft_digest = draft_digest(
            "original-model",
            EffortSelection::Default,
            "different original draft",
        );
        assert_ne!(frame_digest(&foreign, &bytes), ticket.binding.frame_digest);
    }
}
