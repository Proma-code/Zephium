//! BrowserV1 reservation on the existing native worker/queue. Public Page
//! admission requires the current native capability and exact pre-say proof.
use super::*;
use zephium_codex_broker::connection::{BrowserCommit, BrowserDraft, PreparedBrowser};

/// The queue grants only capacity. The original owner produces the no-write
/// proof after its current account/catalog/durable-policy/history checks.
pub(super) struct Prepare {
    pub(super) account_revision: u64,
    pub(super) selection_revision: u64,
    pub(super) draft: BrowserDraft,
    pub(super) manifest: zephium_codex_broker::browser_port::BrowserManifest,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) deadline: Instant,
    pub(super) result: tokio::sync::oneshot::Sender<Result<PreparedBrowser, Fault>>,
}

pub(super) struct Start {
    pub(super) proof: PreparedBrowser,
    pub(super) port: Box<dyn zephium_codex_broker::browser_port::BrowserPort>,
}

struct BrowserReservation {
    base: Reservation,
    proof: PreparedBrowser,
}

pub(super) async fn reserve(
    profile: ProfileId,
    expected: NativeConversationChoice,
    draft: NativeConversationDraft,
    manifest: NativeBrowserManifest,
) -> Result<Box<dyn NativeConversationReservation>, PageBackendError> {
    let base = reserve_page_value(profile, expected, draft, admission::TurnMode::Browser)?;
    reserve_held(
        base,
        manifest,
        Arc::new(AtomicBool::new(false)),
        Instant::now() + Duration::from_secs(8),
    )
    .await
}

pub(super) async fn reserve_held(
    base: Reservation,
    manifest: NativeBrowserManifest,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Box<dyn NativeConversationReservation>, PageBackendError> {
    let native_manifest = browser::manifest(manifest).map_err(|_| {
        PageBackendError::NativeConversation(PageAssistNativeFaultV1::UnsupportedInput)
    })?;
    let key = ConversationKey::from_native(base.draft.conversation.clone()).ok_or(
        PageBackendError::NativeConversation(PageAssistNativeFaultV1::Protocol),
    )?;
    let mut guard = CancelReply(Some(cancelled.clone()));
    let (send, receive) = tokio::sync::oneshot::channel();
    let reserved = base.reserved.as_ref().ok_or(PageBackendError::Busy)?;
    base.service
        .queue
        .inspect_reserved(
            reserved,
            Command::PrepareBrowser(Box::new(Prepare {
                account_revision: base.account_revision,
                selection_revision: base.selection_revision,
                draft: BrowserDraft {
                    key,
                    new_conversation: base.draft.new_conversation,
                    previous_reply: base.draft.previous_reply.map(u64::from),
                    model: base.selection.model.clone(),
                    effort: selected_effort(base.selection.effort),
                    text: base.draft.text.clone(),
                },
                manifest: native_manifest,
                cancelled,
                deadline: deadline.min(Instant::now() + Duration::from_secs(8)),
                result: send,
            })),
        )
        .map_err(|_| PageBackendError::Busy)?;
    // Page owns the outer pre-say timeout. Drop cancels this exact inspection;
    // its result is data-only and cannot retain a provider dispatch obligation.
    let proof = receive
        .await
        .map_err(|_| PageBackendError::NativeConversation(PageAssistNativeFaultV1::Unavailable))?
        .map_err(|fault| {
            if fault == Fault::Busy {
                PageBackendError::Busy
            } else {
                PageBackendError::NativeConversation(page_fault(fault))
            }
        })?;
    base.validate()
        .map_err(PageBackendError::NativeConversation)?;
    guard.0 = None;
    Ok(Box::new(BrowserReservation { base, proof }))
}

/// Called only by the existing owner after receiving the exact reserved
/// inspection command. No provider request or Store mutation is permitted.
pub(super) fn prepare(
    service: &Arc<Service>,
    connection: &mut Connection,
    scope: &conversation::Scope,
    call: Prepare,
) {
    let current = || {
        !call.cancelled.load(Ordering::Acquire)
            && Instant::now() < call.deadline
            && !service.stop.load(Ordering::Acquire)
            && call.account_revision != 0
            && call.account_revision == service.account_revision.load(Ordering::Acquire)
            && call.selection_revision == service.selection_revision.load(Ordering::Acquire)
            && REGISTRY
                .get()
                .is_some_and(|registry| registry.current_now(service))
    };
    if call.result.is_closed() || !current() {
        let _ = call.result.send(Err(Fault::ScopeChanged));
        return;
    }
    let result = connection.prepare_browser(scope, call.draft, call.manifest, &call.cancelled);
    // The native read may have overlapped a foreground/account/choice change.
    // A completed read is not current admission and cannot reopen that source.
    let result = if current() {
        result
    } else {
        Err(Fault::ScopeChanged)
    };
    service
        .queue
        .set_ready(result.is_ok() && connection.can_admit_browser(scope));
    let _ = call.result.send(result);
}

impl NativeConversationReservation for BrowserReservation {
    fn validate(&self) -> Result<(), PageAssistNativeFaultV1> {
        self.base.validate()
    }

    fn commit(
        self: Box<Self>,
        _reply: u32,
        _cancel: Arc<AtomicBool>,
        _events: NativeConversationEvents,
    ) -> BoxFuture<'static, Result<NativeConversationOutcome, PageAssistNativeFaultV1>> {
        Box::pin(async { Err(PageAssistNativeFaultV1::UnsupportedInput) })
    }

    fn commit_browser(
        self: Box<Self>,
        reply: u32,
        cancel: Arc<AtomicBool>,
        events: NativeConversationEvents,
        page: NativeBrowserBridge,
    ) -> BoxFuture<'static, Result<NativeBrowserCompletion, PageAssistNativeFaultV1>> {
        let Self { mut base, proof } = *self;
        let Some(reserved) = base.reserved.take() else {
            return Box::pin(async { Err(PageAssistNativeFaultV1::OutcomeUnknown) });
        };
        let guard = CancelReply(Some(cancel.clone()));
        let (send, receive) = tokio::sync::oneshot::channel();
        let request = NativeConversationRequest {
            conversation: base.draft.conversation,
            reply,
            new_conversation: base.draft.new_conversation,
            previous_reply: base.draft.previous_reply,
            text: base.draft.text,
        };
        let sent = base.service.queue.handoff(reserved, |turn| {
            Command::Reply(ReplyCall {
                turn: Some(turn),
                account_revision: base.account_revision,
                request,
                selection: base.selection,
                events,
                cancel,
                result: ReplySink::Browser(send),
                browser: Some(Box::new(Start {
                    proof,
                    port: Box::new(browser::PageBrowserPort::new(page)),
                })),
            })
        });
        // This future never enqueues. The exact newly committed reply is
        // already actor-owned even if the waiter is never polled or is dropped.
        Box::pin(async move {
            let mut guard = guard;
            sent.map_err(|_| PageAssistNativeFaultV1::OutcomeUnknown)?;
            let result = receive
                .await
                .map_err(|_| PageAssistNativeFaultV1::OutcomeUnknown)?;
            guard.0 = None;
            let registry = REGISTRY
                .get()
                .ok_or(PageAssistNativeFaultV1::ScopeChanged)?;
            result.receive(
                registry.scope(&base.service.profile.to_string()) == Some(base.service.epoch),
                registry.current(&base.service)
                    && base.account_revision
                        == base.service.account_revision.load(Ordering::Acquire),
            )
        })
    }
}

pub(super) fn committed(
    start: Start,
    reply: u32,
    admission: TextAdmission,
) -> (PreparedBrowser, BrowserCommit) {
    (
        start.proof,
        BrowserCommit {
            reply: u64::from(reply),
            admission,
            port: start.port,
        },
    )
}
