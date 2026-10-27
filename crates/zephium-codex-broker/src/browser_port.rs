//! Native Page adapter contract. No renderer endpoint, browser executor, model
//! client, or alternative tool implementation lives here. Every poll is bounded
//! and nonblocking; the existing serialized native owner retains this port.
//!
//! This source is deliberately unmounted until the BrowserV1 owner integration
//! and its durable policy contract are qualified together.
use crate::dynamic_tools::{Definition, NativeManifest, PageTool, MAX_RESULT_BYTES};
use serde_json::Value;
use std::{sync::Arc, time::Instant};

pub const MAX_COMPOSED_INPUT_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserPortFault {
    Cancelled,
    ScopeChanged,
    ConnectionLost,
    Protocol,
    Limit,
    Preparation,
    Budget,
}

/// Translated only from the existing Page `WorkModelTool` definitions by the
/// native adapter. No IPC schema, expected digest, or tool-name override exists.
pub struct BrowserToolDefinition {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// Immutable native manifest and fixed Page instruction policy. User text,
/// readouts and runtime version are never part of this policy identity.
pub struct BrowserManifest {
    tools: NativeManifest,
    instructions: String,
    policy: BrowserPolicy,
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct BrowserPolicy {
    tools: [u8; 32],
    instructions: [u8; 32],
}
impl BrowserManifest {
    pub fn from_native_page_v1(
        instructions: String,
        definitions: Vec<BrowserToolDefinition>,
    ) -> Result<Self, BrowserPortFault> {
        let instruction_digest = NativeManifest::instruction_digest(&instructions)
            .map_err(|_| BrowserPortFault::Limit)?;
        let tools = NativeManifest::page_v1(
            definitions
                .into_iter()
                .map(|definition| Definition {
                    name: definition.name,
                    description: definition.description,
                    input_schema: definition.schema,
                })
                .collect(),
        )
        .map_err(|_| BrowserPortFault::Protocol)?;
        let policy = BrowserPolicy {
            tools: tools.digest(),
            instructions: instruction_digest,
        };
        Ok(Self {
            tools,
            instructions,
            policy,
        })
    }
    pub(crate) fn tools(&self) -> &NativeManifest {
        &self.tools
    }
    pub(crate) fn instructions(&self) -> &str {
        &self.instructions
    }
    pub(crate) fn policy(&self) -> BrowserPolicy {
        self.policy
    }
}
impl BrowserPolicy {
    pub(crate) fn tool_digest(self) -> [u8; 32] {
        self.tools
    }
    pub(crate) fn instruction_digest(self) -> [u8; 32] {
        self.instructions
    }
}

/// The Page owner supplies its complete, newly prepared input exactly once.
/// The broker still runs its original full-input skill-denial validator before
/// refining the write intent. This is not a prevalidated model-write permit.
pub struct BrowserInput(String);
impl BrowserInput {
    pub fn from_native_page(text: String) -> Result<Self, BrowserPortFault> {
        if text.is_empty() || text.len() > MAX_COMPOSED_INPUT_BYTES {
            return Err(BrowserPortFault::Limit);
        }
        Ok(Self(text))
    }
    pub(crate) fn into_text(self) -> String {
        self.0
    }
}
pub enum BrowserInputPoll {
    Pending,
    Ready(BrowserInput),
    Failed(BrowserPortFault),
}

/// Private allocation identity joins one port to all of its dispatch/result
/// tickets. Equal ordinals or argument digests on a different bridge do not join.
#[derive(Clone)]
pub(crate) struct BrowserStamp(Arc<()>);
impl BrowserStamp {
    pub(crate) fn new() -> Self {
        Self(Arc::new(()))
    }
}
/// Move-only native call. Only the ledger-owning actor can construct this.
pub struct BrowserDispatch {
    key: BrowserResultKey,
    tool: PageTool,
    arguments: Value,
}
/// Retain beside the Page oneshot receiver. Consume only after that exact Page
/// future actually returns. A receiver error is NOT an unknown-effect receipt.
pub struct BrowserResultKey {
    stamp: BrowserStamp,
    ordinal: u8,
    body_digest: [u8; 32],
}
pub struct BrowserResult {
    key: BrowserResultKey,
    text: String,
    success: bool,
}
impl BrowserDispatch {
    pub(crate) fn new(
        stamp: BrowserStamp,
        ordinal: u8,
        tool: PageTool,
        arguments: Value,
        body_digest: [u8; 32],
    ) -> Option<Self> {
        (ordinal != 0 && ordinal <= 40 && body_digest != [0; 32]).then_some(Self {
            key: BrowserResultKey {
                stamp,
                ordinal,
                body_digest,
            },
            tool,
            arguments,
        })
    }
    pub fn ordinal(&self) -> u8 {
        self.key.ordinal
    }
    pub fn name(&self) -> &'static str {
        self.tool.name()
    }
    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
    /// The native adapter keeps the unforgeable result key with its Page call.
    pub fn into_parts(self) -> (u8, &'static str, Value, BrowserResultKey) {
        (self.key.ordinal, self.tool.name(), self.arguments, self.key)
    }
    pub(crate) fn matches(&self, stamp: &BrowserStamp, ordinal: u8, body_digest: [u8; 32]) -> bool {
        self.key.matches(stamp, ordinal, body_digest)
    }
}
impl BrowserResultKey {
    fn matches(&self, stamp: &BrowserStamp, ordinal: u8, body_digest: [u8; 32]) -> bool {
        Arc::ptr_eq(&self.stamp.0, &stamp.0)
            && self.ordinal == ordinal
            && self.body_digest == body_digest
    }
    /// This conversion is called only with the actual Page tool output. On a
    /// bound failure the same key remains owned; no result/settlement is minted.
    pub fn complete(self, text: String, success: bool) -> Result<BrowserResult, Self> {
        if text.len() > MAX_RESULT_BYTES {
            return Err(self);
        }
        Ok(BrowserResult {
            key: self,
            text,
            success,
        })
    }
}
impl BrowserResult {
    pub(crate) fn consume(
        self,
        stamp: &BrowserStamp,
        ordinal: u8,
        body_digest: [u8; 32],
    ) -> Result<(String, bool), Self> {
        if !self.key.matches(stamp, ordinal, body_digest) {
            return Err(self);
        }
        Ok((self.text, self.success))
    }
}
pub enum BrowserDispatchOutcome {
    Accepted,
    /// Exact unchanged call, before admission to the Page channel. This says
    /// nothing about any earlier effect or the original model turn.
    Refused(BrowserDispatch),
}
pub enum BrowserResultPoll {
    Pending,
    Ready(BrowserResult),
    /// Retain the ledger/effect obligation until the independent local receipt.
    Lost,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserRevokeReason {
    ScopeChanged,
    ConnectionLost,
    Protocol,
    Cancelled,
}
/// Passed only when native tool admission is irreversibly closed. The native
/// adapter retains this exact key beside the same Page settlement receiver and
/// may consume it only after receiving the private Page receipt.
pub struct BrowserCompletionKey {
    stamp: BrowserStamp,
    entered: Vec<(u8, [u8; 32])>,
}
pub struct BrowserLocalReceipt(BrowserCompletionKey);
impl BrowserCompletionKey {
    pub(crate) fn new(stamp: BrowserStamp, entered: Vec<(u8, [u8; 32])>) -> Option<Self> {
        if entered.len() > 40
            || entered
                .iter()
                .any(|(ordinal, digest)| *ordinal == 0 || *ordinal > 40 || *digest == [0; 32])
            || entered.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        {
            return None;
        }
        Some(Self { stamp, entered })
    }
    /// Trusted adapter conversion after the ACTUAL private Page receipt, never
    /// after channel closure, cancellation, timeout or provider completion.
    pub fn observed(self) -> BrowserLocalReceipt {
        BrowserLocalReceipt(self)
    }
}
impl BrowserLocalReceipt {
    pub(crate) fn consume(
        self,
        stamp: &BrowserStamp,
        entered: &[(u8, [u8; 32])],
    ) -> Result<(), Self> {
        if !Arc::ptr_eq(&self.0.stamp.0, &stamp.0) || self.0.entered != entered {
            return Err(self);
        }
        Ok(())
    }
}
pub enum BrowserLocalSettlement {
    Pending,
    /// Only the actual private Page LocalSettlement receipt may produce this.
    /// Neither a closed sender, dropped waiter, timeout nor provider terminal can.
    Observed(BrowserLocalReceipt),
    /// Quarantine/retain; this never means local effects settled successfully.
    Lost,
}

/// Trusted native adapter over one exact Page bridge allocation. It is not an
/// extension point exposed to the renderer. Methods must not block, run tools,
/// await user input, renew a budget, or synthesize a settlement receipt.
/// Read-only view of the SAME Page cumulative budget. Implementations may only
/// read that budget's private mutex; no callbacks, provider work or pause rights.
pub trait BrowserBudget: Send + Sync {
    fn deadline(&self) -> Result<Instant, BrowserPortFault>;
    fn hard_deadline(&self) -> Result<Instant, BrowserPortFault>;
}
pub trait BrowserPort: Send {
    fn poll_input(&mut self) -> BrowserInputPoll;
    fn dispatch(&mut self, call: BrowserDispatch) -> BrowserDispatchOutcome;
    fn poll_result(&mut self) -> BrowserResultPoll;
    fn budget(&self) -> Arc<dyn BrowserBudget>;
    fn revoke(&mut self, reason: BrowserRevokeReason);
    /// Drop the SOLE tool-request sender before waiting for local settlement.
    /// Normal authoritative terminal closes only; authority loss also revokes.
    fn close_admission(&mut self, completion: BrowserCompletionKey);
    fn poll_local_settlement(&mut self) -> BrowserLocalSettlement;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn dispatch(stamp: BrowserStamp) -> BrowserDispatch {
        BrowserDispatch::new(
            stamp,
            1,
            PageTool::ReadPage,
            json!({"target":"current"}),
            [7; 32],
        )
        .unwrap()
    }
    #[test]
    fn exact_bridge_ordinal_and_argument_digest_required() {
        let stamp = BrowserStamp::new();
        let call = dispatch(stamp.clone());
        assert!(call.matches(&stamp, 1, [7; 32]));
        assert!(!call.matches(&BrowserStamp::new(), 1, [7; 32]));
        assert!(!call.matches(&stamp, 2, [7; 32]));
        assert!(!call.matches(&stamp, 1, [8; 32]));
        let (ordinal, name, arguments, key) = call.into_parts();
        assert_eq!((ordinal, name), (1, "read_page"));
        assert_eq!(arguments, json!({"target":"current"}));
        let result = key
            .complete("actual Page result".into(), true)
            .ok()
            .unwrap();
        let result = result
            .consume(&BrowserStamp::new(), 1, [7; 32])
            .err()
            .unwrap();
        assert_eq!(
            result.consume(&stamp, 1, [7; 32]).ok().unwrap(),
            ("actual Page result".into(), true)
        );
    }
    #[test]
    fn result_bound_retains_same_key_and_failed_result_is_not_success() {
        let stamp = BrowserStamp::new();
        let (_, _, _, key) = dispatch(stamp.clone()).into_parts();
        let key = key
            .complete("x".repeat(MAX_RESULT_BYTES + 1), true)
            .err()
            .unwrap();
        let result = key.complete("Page refusal".into(), false).ok().unwrap();
        assert_eq!(
            result.consume(&stamp, 1, [7; 32]).ok().unwrap(),
            ("Page refusal".into(), false)
        );
    }
    #[test]
    fn complete_input_bounds_are_not_plain_draft_validation() {
        assert!(BrowserInput::from_native_page(String::new()).is_err());
        assert!(BrowserInput::from_native_page("x".repeat(MAX_COMPOSED_INPUT_BYTES + 1)).is_err());
        let text = "x".repeat(MAX_COMPOSED_INPUT_BYTES);
        assert_eq!(
            BrowserInput::from_native_page(text.clone())
                .unwrap_or_else(|_| panic!())
                .into_text(),
            text
        );
    }
    #[test]
    fn native_policy_hashes_both_fixed_components() {
        let definitions = || {
            PageTool::ALL
                .into_iter()
                .map(|tool| BrowserToolDefinition {
                    name: tool.name().into(),
                    description: "original Page definition".into(),
                    schema: json!({"type":"object"}),
                })
                .collect()
        };
        let first =
            BrowserManifest::from_native_page_v1("fixed Page instructions".into(), definitions())
                .unwrap_or_else(|_| panic!());
        let other = BrowserManifest::from_native_page_v1(
            "different fixed instructions".into(),
            definitions(),
        )
        .unwrap_or_else(|_| panic!());
        assert_eq!(first.policy().tool_digest(), other.policy().tool_digest());
        assert_ne!(
            first.policy().instruction_digest(),
            other.policy().instruction_digest()
        );
        assert_eq!(first.instructions(), "fixed Page instructions");
        assert!(first.tools().contains(PageTool::HandOff));
    }
    #[test]
    fn aggregate_receipt_covers_only_same_bridge_and_exact_entered_set() {
        let stamp = BrowserStamp::new();
        let entered = vec![(1, [7; 32]), (3, [8; 32])];
        let receipt = BrowserCompletionKey::new(stamp.clone(), entered.clone())
            .unwrap()
            .observed();
        let receipt = receipt
            .consume(&BrowserStamp::new(), &entered)
            .err()
            .unwrap();
        let receipt = receipt.consume(&stamp, &[(1, [7; 32])]).err().unwrap();
        let receipt = receipt
            .consume(&stamp, &[(1, [7; 32]), (3, [9; 32])])
            .err()
            .unwrap();
        assert!(receipt.consume(&stamp, &entered).is_ok());
        // No Clone / second receipt constructor exists. A duplicate ordinal
        // cannot be represented as another entered effect in a completion key.
        assert!(
            BrowserCompletionKey::new(stamp.clone(), vec![(1, [7; 32]), (1, [7; 32])]).is_none()
        );
        assert!(BrowserCompletionKey::new(stamp, vec![(0, [7; 32])]).is_none());
    }
}
