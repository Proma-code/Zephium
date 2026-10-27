//! Native browser protocol data. These are host-created ports, not renderer
//! commands or substitutes for Page's original tool/approval authority.
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot, Notify};
use zephium_core::work::model::WorkModelTool;
use zephium_ipc::page_assist::PageAssistNativeFaultV1 as Fault;

use super::native_budget::NativeBudgetView;

pub const NATIVE_BROWSER_INPUT_BYTES: usize = 128 * 1024;
pub const NATIVE_BROWSER_ARGUMENT_BYTES: usize = 64 * 1024;
pub const NATIVE_BROWSER_RESULT_BYTES: usize = 128 * 1024;
pub const NATIVE_BROWSER_TOOL_CALLS: u32 = 40;
const QUEUED_TOOLS: usize = 8;

/// Exact Page-owned manifest data. The native transport must validate the nine
/// fixed names/schemas, compute its canonical digest, and bind that digest to
/// the actual durable thread. This value alone is never dispatch authority.
pub struct NativeBrowserManifest {
    tools: Vec<WorkModelTool>,
}

impl NativeBrowserManifest {
    pub(super) fn page_v1() -> Self {
        Self {
            tools: super::tools::definitions(),
        }
    }

    pub fn version(&self) -> &'static str {
        "zephium-page-browser-v1"
    }

    pub fn instructions(&self) -> &'static str {
        zephium_page_assist::prompt::SYSTEM
    }

    pub fn tools(&self) -> &[WorkModelTool] {
        &self.tools
    }
}

/// The newly admitted, bounded current request plus Page's fenced context. No
/// past conversation replay, model choice, tool schema, or provider thread ID.
pub struct NativeBrowserInput {
    text: String,
}

impl NativeBrowserInput {
    pub(super) fn new(text: String) -> Result<Self, Fault> {
        if text.is_empty() || text.len() > NATIVE_BROWSER_INPUT_BYTES {
            return Err(Fault::Limit);
        }
        Ok(Self { text })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }
}

/// One physically returned original Page tool outcome; unknown/cancelled
/// effects are not fabricated as either successful or safely unexecuted.
pub struct NativeBrowserToolOutput {
    text: String,
    success: bool,
}

impl NativeBrowserToolOutput {
    pub(super) fn new(result: Result<String, String>) -> Result<Self, Fault> {
        let (text, success) = match result {
            Ok(text) => (text, true),
            Err(text) => (text, false),
        };
        if text.len() > NATIVE_BROWSER_RESULT_BYTES {
            return Err(Fault::Limit);
        }
        Ok(Self { text, success })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn success(&self) -> bool {
        self.success
    }
}

/// A transport-owned original request's one-shot local dispatch. It cannot be
/// cloned; the native owner retains all original wire/thread/turn/call IDs and
/// must join this response to its exact pending ledger entry, never by name.
pub struct NativeBrowserToolRequest {
    pub(super) ordinal: u32,
    pub(super) name: String,
    pub(super) arguments: Value,
    pub(super) response: oneshot::Sender<Result<NativeBrowserToolOutput, Fault>>,
}

impl NativeBrowserToolRequest {
    /// Native adapters only: allocation is not permission to execute. Page
    /// rechecks the sequential ordinal, scope, budget and closed parser.
    pub fn channel(
        ordinal: u32,
        name: String,
        arguments: Value,
    ) -> Result<
        (
            Self,
            oneshot::Receiver<Result<NativeBrowserToolOutput, Fault>>,
        ),
        Fault,
    > {
        if !(1..=NATIVE_BROWSER_TOOL_CALLS).contains(&ordinal)
            || name.len() > 128
            || super::tools::known(&name) == "unknown"
            || serde_json::to_vec(&arguments)
                .map_err(|_| Fault::Protocol)?
                .len()
                > NATIVE_BROWSER_ARGUMENT_BYTES
        {
            return Err(Fault::Protocol);
        }
        let (response, receiver) = oneshot::channel();
        Ok((
            Self {
                ordinal,
                name,
                arguments,
                response,
            },
            receiver,
        ))
    }
}

/// The already-committed native owner waits for prepared input without losing
/// its reserved slot. Input cancellation/drop must settle the original intent
/// as NeverStarted only after the actual writer/Skip proof; not by assumption.
pub struct NativeBrowserBridge {
    prepared: oneshot::Receiver<Result<NativeBrowserInput, Fault>>,
    requests: mpsc::Sender<NativeBrowserToolRequest>,
    timing: NativeBudgetView,
    settled: oneshot::Receiver<NativeBrowserLocalSettlement>,
    revoke: NativeBrowserRevocation,
}

/// Browser-only physical disposition, independent of the original provider
/// terminal. Quarantine is historical evidence, never a joined success or a
/// permission to release the native slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeBrowserSettlement {
    Joined { local: NativeBrowserLocalOutcome },
    Quarantined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeBrowserLocalOutcome {
    Observed,
    ResultUnobserved,
}

pub struct NativeBrowserCompletion {
    pub outcome: super::port::NativeConversationOutcome,
    pub settlement: NativeBrowserSettlement,
}

/// One irreversible no-new-action gate. Only its native handle can close it;
/// Page observes the same allocation at every action and actual human wait.
/// Closure does not cancel the provider, invent a fault, or renew a budget.
#[derive(Default)]
pub(super) struct NativeBrowserAdmission {
    closed: AtomicBool,
    changed: Notify,
}
impl NativeBrowserAdmission {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }
    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    pub(super) async fn closed(&self) {
        loop {
            let changed = self.changed.notified();
            if self.is_closed() {
                return;
            }
            changed.await;
        }
    }
}

/// Native authority loss can only revoke this exact Page bridge. It cannot
/// approve a tool, pause a budget, renew scope, or mint a no-send receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeBrowserRevokeReason {
    ScopeChanged,
    ConnectionLost,
    Protocol,
    Cancelled,
}

pub struct NativeBrowserRevocation {
    admission: Arc<NativeBrowserAdmission>,
    revoke: Box<dyn Fn(NativeBrowserRevokeReason) + Send + Sync>,
}
impl NativeBrowserRevocation {
    pub(super) fn new(
        admission: Arc<NativeBrowserAdmission>,
        revoke: impl Fn(NativeBrowserRevokeReason) + Send + Sync + 'static,
    ) -> Self {
        Self {
            admission,
            revoke: Box::new(revoke),
        }
    }
    /// Normal original terminal closes future Page action admission and wakes
    /// an actual pending approval, without labeling an idle success cancelled.
    pub fn close_admission(&self) {
        self.admission.close();
    }
    pub fn revoke(&self, reason: NativeBrowserRevokeReason) {
        self.close_admission();
        (self.revoke)(reason);
    }
}

/// Only the Page dispatcher owner can mint this after all entered local work
/// has returned. Receiver closure, provider terminal, and Stop are not receipts.
pub struct NativeBrowserLocalSettlement {
    _private: (),
}

impl NativeBrowserLocalSettlement {
    pub(super) fn returned() -> Self {
        Self { _private: () }
    }
}

/// Native owner parts. Retain the revocation-only wake for every account/scope
/// invalidation; close the request sender before awaiting local settlement.
pub type NativeBrowserParts = (
    oneshot::Receiver<Result<NativeBrowserInput, Fault>>,
    mpsc::Sender<NativeBrowserToolRequest>,
    NativeBudgetView,
    oneshot::Receiver<NativeBrowserLocalSettlement>,
    NativeBrowserRevocation,
);

impl NativeBrowserBridge {
    pub fn into_parts(self) -> NativeBrowserParts {
        (
            self.prepared,
            self.requests,
            self.timing,
            self.settled,
            self.revoke,
        )
    }
}

pub(super) struct PageBrowserBridge {
    pub(super) prepared: oneshot::Sender<Result<NativeBrowserInput, Fault>>,
    pub(super) requests: mpsc::Receiver<NativeBrowserToolRequest>,
    pub(super) settled: oneshot::Sender<NativeBrowserLocalSettlement>,
}

pub(super) fn bridge(
    timing: NativeBudgetView,
    revoke: NativeBrowserRevocation,
) -> (PageBrowserBridge, NativeBrowserBridge) {
    let (prepared, receiver) = oneshot::channel();
    let (requests, incoming) = mpsc::channel(QUEUED_TOOLS);
    let (settled, returned) = oneshot::channel();
    (
        PageBrowserBridge {
            prepared,
            requests: incoming,
            settled,
        },
        NativeBrowserBridge {
            prepared: receiver,
            requests,
            timing,
            settled: returned,
            revoke,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn normal_admission_close_is_sticky_wakes_and_does_not_revoke() {
        let admission = Arc::new(NativeBrowserAdmission::default());
        let revoked = Arc::new(AtomicBool::new(false));
        let recorded = revoked.clone();
        let control = NativeBrowserRevocation::new(admission.clone(), move |_| {
            recorded.store(true, Ordering::Release);
        });
        assert!(!admission.is_closed());
        control.close_admission();
        control.close_admission();
        admission.closed().await;
        assert!(admission.is_closed());
        assert!(!revoked.load(Ordering::Acquire));
        control.revoke(NativeBrowserRevokeReason::ScopeChanged);
        assert!(revoked.load(Ordering::Acquire));
        assert!(admission.is_closed());
    }

    #[test]
    fn manifest_is_the_original_nine_tools_and_instructions() {
        let manifest = NativeBrowserManifest::page_v1();
        assert_eq!(manifest.version(), "zephium-page-browser-v1");
        assert_eq!(manifest.instructions(), zephium_page_assist::prompt::SYSTEM);
        assert_eq!(manifest.tools().len(), 9);
        for (actual, original) in manifest
            .tools()
            .iter()
            .zip(super::super::tools::definitions())
        {
            assert_eq!(actual.name, original.name);
            assert_eq!(actual.description, original.description);
            assert_eq!(actual.schema, original.schema);
            assert_eq!(actual.strict, original.strict);
        }
    }

    #[test]
    fn prepared_input_is_exact_and_never_silently_truncated() {
        let raw = "  شخص\r\n$50000 `literal`  ";
        assert_eq!(
            NativeBrowserInput::new(raw.into()).unwrap().into_text(),
            raw
        );
        assert!(NativeBrowserInput::new("x".repeat(NATIVE_BROWSER_INPUT_BYTES)).is_ok());
        assert!(matches!(
            NativeBrowserInput::new("x".repeat(NATIVE_BROWSER_INPUT_BYTES + 1)),
            Err(Fault::Limit)
        ));
    }

    #[test]
    fn tool_channel_has_closed_names_and_finite_arguments() {
        assert!(
            NativeBrowserToolRequest::channel(1, "list_tabs".into(), serde_json::json!({})).is_ok()
        );
        assert!(
            NativeBrowserToolRequest::channel(0, "list_tabs".into(), serde_json::json!({}))
                .is_err()
        );
        assert!(
            NativeBrowserToolRequest::channel(41, "list_tabs".into(), serde_json::json!({}))
                .is_err()
        );
        assert!(
            NativeBrowserToolRequest::channel(1, "shell".into(), serde_json::json!({})).is_err()
        );
        assert!(NativeBrowserToolRequest::channel(
            1,
            "read_page".into(),
            serde_json::json!({"tab":"x".repeat(NATIVE_BROWSER_ARGUMENT_BYTES)})
        )
        .is_err());
    }

    #[test]
    fn output_preserves_failure_and_empty_success() {
        let success = NativeBrowserToolOutput::new(Ok(String::new())).unwrap();
        assert!(success.success());
        assert_eq!(success.text(), "");
        let failed = NativeBrowserToolOutput::new(Err("declined".into())).unwrap();
        assert!(!failed.success());
        assert_eq!(failed.text(), "declined");
        assert!(matches!(
            NativeBrowserToolOutput::new(Ok("x".repeat(NATIVE_BROWSER_RESULT_BYTES + 1))),
            Err(Fault::Limit)
        ));
    }
}
