//! One reply, start to finish: read what the person attached, then call the
//! model and run the tools it asks for until it answers. Text streams as it
//! comes, cited and paced; every tool's effect is admitted against the reads
//! of this reply. Every wait races the reply's cancellation.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use zephium_app::work_models::WorkCallSource;
use zephium_core::ids::ProfileId;
use zephium_core::work::model::{
    work_model_cache_key, WorkModelEvent, WorkModelMessage, WorkModelPart, WorkModelRequest,
    WorkModelStop, WorkModelSystemBlock, WorkModelToolChoice, WorkModelToolResult, WorkModelUsage,
};
use zephium_ipc::page_assist::*;
use zephium_page_assist::actions::{Step, Verb};
use zephium_page_assist::outline::Outline;
use zephium_page_assist::pacer::Pacer;
use zephium_page_assist::refs::{self, Ref};
use zephium_page_assist::stream::CiteRewriter;
use zephium_page_assist::{css, guard, prompt, suggest, text};

use super::port::{
    BringRequest, Glow, NativeConversationEvent, NativeConversationReservation,
    NativeConversationStatus, PageBackendChoice, PageBackendError, ReadFailure, ResolvedModel,
    ResolvedPageBackend, Role, StepEffect, StepFailure, StepRequest, WorkStart,
};
#[path = "native_driver.rs"]
mod browser;
pub(super) use browser::{commit_native_browser, BrowserOwner};

use super::tools::{self, Call, ItemCall};
use super::{
    fault, host_of, readout, site_of, Admitted, Cancel, CancelReason, PageAssist, PendingStyle,
    ReadRecord, MAX_LINKS,
};

const READ_TIMEOUT: Duration = Duration::from_secs(8);
const OUTLINE_TIMEOUT: Duration = Duration::from_secs(3);
const CALL_TIMEOUT: Duration = Duration::from_secs(90);
/// Longest a new tab is watched for its first load.
const OPEN_TIMEOUT: Duration = Duration::from_secs(12);
/// Longest a new tab may stay unknown to the shell before it counts as open.
const UNLISTED_TIMEOUT: Duration = Duration::from_millis(1_500);
const TEXT_INTERVAL_MS: u64 = 50;
/// Most model calls one reply makes.
const MAX_CALLS: usize = 16;
/// Most tool calls one reply runs.
const MAX_TOOLS: usize = 40;
/// Most pages one reply reads.
const MAX_READS: usize = 12;
/// Longest transcript handed to Work, in characters.
const MAX_TRANSCRIPT_CHARS: usize = 6_000;

pub(super) struct Reply {
    pub(super) service: PageAssist,
    pub(super) conversation: String,
    pub(super) space: String,
    pub(super) profile: ProfileId,
    /// Exact foreground lifetime captured before the native reservation.
    pub(super) current_epoch: Option<Arc<()>>,
    pub(super) reply: u32,
    pub(super) words: String,
    pub(super) native: std::sync::Mutex<Option<NativeTurn>>,
    pub(super) native_budget: Option<super::native_budget::NativeBudget>,
    pub(super) native_admission: Option<Arc<super::native_browser::NativeBrowserAdmission>>,
    pub(super) backend: PageBackendChoice,
    pub(super) suggestion: Option<PageAssistSuggestionV1>,
    pub(super) attachments: Vec<Admitted>,
    pub(super) cancel: Arc<Cancel>,
    pub(super) leased: Arc<AtomicBool>,
    pub(super) took_over: Arc<AtomicBool>,
    /// Tabs lit while this reply works on them.
    pub(super) glowing: std::sync::Mutex<Vec<String>>,
}

enum End {
    /// An exact native result was already projected atomically.
    NativeProjected,
    Done(Option<PageAssistCostV1>),
    Failed(PageAssistFaultV1),
    Cancelled(CancelReason),
}

/// The open text block and what streams into it.
struct Text {
    block: Option<u16>,
    rewriter: CiteRewriter,
    pacer: Pacer<String>,
    cites: Vec<PageAssistCiteV1>,
    sent_cites: usize,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            block: None,
            rewriter: CiteRewriter::default(),
            pacer: Pacer::new(TEXT_INTERVAL_MS),
            cites: Vec::new(),
            sent_cites: 0,
        }
    }
}

type ToolOutcome = Result<String, String>;

impl Reply {
    fn apply(&self, kind: PageAssistEventV1) -> bool {
        if self.native_budget.is_some()
            && self.cancel.reason().is_some()
            && !matches!(
                &kind,
                PageAssistEventV1::Failed { .. }
                    | PageAssistEventV1::Cancelled
                    | PageAssistEventV1::Phase { phase: None }
            )
        {
            return false;
        }
        if !matches!(self.backend, PageBackendChoice::NativeConversation(_)) {
            return self.service.apply(&self.conversation, self.reply, kind);
        }
        let event = {
            let mut state = self.service.state();
            if self.native_budget.is_some()
                && (state.current.get(&self.space) != Some(&self.conversation)
                    || self
                        .current_epoch
                        .as_ref()
                        .zip(state.current_epochs.get(&self.space))
                        .is_none_or(|(captured, current)| !Arc::ptr_eq(captured, current))
                    || state
                        .live
                        .get(&self.conversation)
                        .is_none_or(|live| live.profile != self.profile))
            {
                return false;
            }
            if state.running.get(&self.conversation).is_none_or(|running| {
                running.reply != self.reply || !Arc::ptr_eq(&running.cancel, &self.cancel)
            }) {
                return false;
            }
            state.live.get_mut(&self.conversation).and_then(|live| {
                let event = live.conversation.apply(self.reply, kind);
                if event.is_some() {
                    live.revision = Arc::new(());
                }
                event
            })
        };
        let sent = event.is_some();
        self.service.emit_all(event);
        sent
    }

    fn note(&self, fact: std::fmt::Arguments<'_>) {
        self.service.ports().diagnostics.note(format_args!(
            "assist: conversation={} reply={} {fact}",
            self.conversation, self.reply
        ));
    }

    fn glow(&self, tab: &str, glow: Glow) {
        let mut glowing = self.glowing.lock().unwrap_or_else(|p| p.into_inner());
        if glow != Glow::Off && !glowing.iter().any(|known| known == tab) {
            glowing.push(tab.to_owned());
        }
        drop(glowing);
        self.service.ports().page.glow(tab, glow);
    }

    fn unglow(&self) {
        let tabs = std::mem::take(&mut *self.glowing.lock().unwrap_or_else(|p| p.into_inner()));
        for tab in tabs {
            self.service.ports().page.glow(&tab, Glow::Off);
        }
    }

    fn phase(&self, phase: Option<PageAssistPhaseV1>) {
        self.apply(PageAssistEventV1::Phase { phase });
    }

    /// Check again at every effect boundary, not just when a provider proposed it.
    fn browser_admission(&self) -> Result<(), PageAssistNativeFaultV1> {
        let Some(budget) = &self.native_budget else {
            return Ok(());
        };
        if self.cancel.reason().is_some() || self.browser_closed() {
            return Err(PageAssistNativeFaultV1::Cancelled);
        }
        if !self.service.ports().settings.ai_enabled(&self.profile) {
            return Err(PageAssistNativeFaultV1::ScopeChanged);
        }
        let state = self.service.state();
        if state.current.get(&self.space) != Some(&self.conversation)
            || self
                .current_epoch
                .as_ref()
                .zip(state.current_epochs.get(&self.space))
                .is_none_or(|(captured, current)| !Arc::ptr_eq(captured, current))
            || state
                .live
                .get(&self.conversation)
                .is_none_or(|live| live.profile != self.profile)
            || state.running.get(&self.conversation).is_none_or(|running| {
                running.reply != self.reply || !Arc::ptr_eq(&running.cancel, &self.cancel)
            })
        {
            return Err(PageAssistNativeFaultV1::ScopeChanged);
        }
        drop(state);
        budget
            .view()
            .deadline()
            .map(|_| ())
            .map_err(|_| PageAssistNativeFaultV1::Timeout)
    }

    fn browser_closed(&self) -> bool {
        self.native_admission
            .as_ref()
            .is_some_and(|admission| admission.is_closed())
    }

    async fn browser_closed_wait(&self) {
        match &self.native_admission {
            Some(admission) => admission.closed().await,
            None => std::future::pending().await,
        }
    }

    fn browser_effect(&self) -> ToolOutcome {
        self.browser_admission()
            .map(|_| String::new())
            .map_err(|_| "This Page reply can no longer perform browser actions.".to_owned())
    }

    fn now(&self) -> u64 {
        self.service.ports().clock.now_ms()
    }

    fn with_data<T>(&self, apply: impl FnOnce(&mut super::ReplyData) -> T) -> Option<T> {
        let mut state = self.service.state();
        state
            .live
            .get_mut(&self.conversation)
            .and_then(|live| live.replies.get_mut(&self.reply))
            .map(apply)
    }

    fn next_block(&self) -> u16 {
        let state = self.service.state();
        state
            .live
            .get(&self.conversation)
            .map_or(0, |live| live.conversation.next_block(self.reply))
    }

    fn add_block(&self, text: &mut Text, value: PageAssistBlockV1) -> u16 {
        self.flush(text);
        text.block = None;
        text.cites.clear();
        text.sent_cites = 0;
        let block = self.next_block();
        self.apply(PageAssistEventV1::Block { block, value });
        block
    }

    fn set_block(&self, block: u16, value: PageAssistBlockV1) {
        self.apply(PageAssistEventV1::Block { block, value });
    }

    fn admit_links(&self, links: impl IntoIterator<Item = String>) {
        let mut state = self.service.state();
        if let Some(live) = state.live.get_mut(&self.conversation) {
            for link in links {
                if live.links.len() >= MAX_LINKS {
                    break;
                }
                if zephium_page_assist::policy::is_web_url(&link) {
                    live.links.insert(link);
                }
            }
        }
    }

    fn handle(&self, tab: &str) -> String {
        self.with_data(|data| {
            let index = match data.handles.iter().position(|known| known == tab) {
                Some(index) => index,
                None => {
                    data.handles.push(tab.to_owned());
                    data.handles.len() - 1
                }
            };
            format!("t{}", index + 1)
        })
        .unwrap_or_default()
    }

    fn tab_of(&self, handle: &str) -> Option<String> {
        let index: usize = handle.trim().strip_prefix('t')?.parse().ok()?;
        self.with_data(|data| data.handles.get(index.checked_sub(1)?).cloned())
            .flatten()
    }

    fn record(&self, letters: &str) -> Option<(usize, ReadRecord)> {
        let index = usize::from(refs::parse(&format!("{letters}1"))?.read);
        self.with_data(|data| data.reads.get(index).cloned().map(|read| (index, read)))
            .flatten()
    }

    fn record_of(&self, reference: Ref) -> Option<ReadRecord> {
        self.with_data(|data| data.reads.get(usize::from(reference.read)).cloned())
            .flatten()
    }

    // ------------------------------------------------------------------ text

    fn deliver(&self, text: &mut Text, chunks: Vec<String>) {
        let delta: String = chunks.concat();
        if !delta.is_empty() {
            let block = *text.block.get_or_insert_with(|| self.next_block());
            self.apply(PageAssistEventV1::Text { block, delta });
        }
        if let Some(block) = text.block {
            if text.cites.len() != text.sent_cites {
                text.sent_cites = text.cites.len();
                self.apply(PageAssistEventV1::Cites {
                    block,
                    cites: text.cites.clone(),
                });
            }
        }
    }

    fn take(&self, text: &mut Text, chunk: &str) {
        let cites = &mut text.cites;
        let rewritten = text.rewriter.push(chunk, &mut |reference| {
            let cite = self
                .with_data(|data| {
                    let read = data.reads.get(usize::from(reference.read))?;
                    let (page, tab, epoch) = (read.page.clone(), read.tab.clone(), read.epoch);
                    data.book.cite(reference, &page, &tab, epoch)
                })
                .flatten()?;
            if !cites.iter().any(|known| known.n == cite.n) {
                cites.push(cite.clone());
            }
            Some(cite.n)
        });
        if !rewritten.is_empty() {
            let now = self.now();
            let due = text.pacer.push(rewritten, now);
            self.deliver(text, due);
        }
    }

    fn flush(&self, text: &mut Text) {
        let rest = text.rewriter.finish();
        if !rest.is_empty() {
            text.pacer.push(rest, self.now());
        }
        let due = text.pacer.flush();
        self.deliver(text, due);
    }

    // ----------------------------------------------------------------- reads

    async fn read_tab(&self, tab: &str) -> Result<usize, PageAssistFaultV1> {
        self.browser_admission()
            .map_err(|fault| PageAssistFaultV1::NativeConversation { fault })?;
        let ports = self.service.ports();
        let facts = ports
            .page
            .facts(tab)
            .filter(|facts| facts.profile == self.profile)
            .ok_or(PageAssistFaultV1::Unavailable)?;
        if let Some(fault) =
            zephium_page_assist::policy::refusal(self.service.availability(Some(&facts)).await)
        {
            return Err(fault);
        }
        let url = facts.url.clone().unwrap_or_default();
        let host = host_of(&url);
        let site = site_of(Some(&url)).ok_or(PageAssistFaultV1::NotWeb)?;
        let index = self
            .with_data(|data| data.reads.len())
            .ok_or(PageAssistFaultV1::Unavailable)?;
        if index >= MAX_READS {
            return Err(PageAssistFaultV1::TooLong);
        }
        let letters = refs::letters(u16::try_from(index).unwrap_or(u16::MAX))
            .ok_or(PageAssistFaultV1::TooLong)?;
        self.phase(Some(PageAssistPhaseV1::Reading { site: host.clone() }));
        // Availability may await native model state; seal/cancel can arrive
        // there, so the actual browser read has its own final admission check.
        self.browser_admission()
            .map_err(|fault| PageAssistFaultV1::NativeConversation { fault })?;
        let readout =
            match tokio::time::timeout(READ_TIMEOUT, ports.page.read(tab, facts.epoch)).await {
                Ok(Ok(readout)) if readout.epoch == facts.epoch => readout,
                Ok(Ok(_)) | Ok(Err(ReadFailure::PageChanged)) => {
                    return Err(PageAssistFaultV1::PageChanged);
                }
                Ok(Err(ReadFailure::NotReady)) => return Err(PageAssistFaultV1::Loading),
                Ok(Err(ReadFailure::Unavailable)) | Err(_) => {
                    return Err(PageAssistFaultV1::Unreadable);
                }
            };
        self.browser_admission()
            .map_err(|fault| PageAssistFaultV1::NativeConversation { fault })?;
        let page = readout::page_read(
            &letters,
            &site,
            &host,
            &readout,
            &zephium_agentic::contains_recognized_secret,
        );
        if page.fragments.is_empty() {
            return Err(PageAssistFaultV1::Unreadable);
        }
        let traits = page.traits;
        let links: Vec<String> = page
            .fragments
            .values()
            .filter_map(|f| f.href.clone())
            .collect();
        {
            let mut state = self.service.state();
            state.traits.insert(tab.to_owned(), (facts.epoch, traits));
            if let Some(live) = state.live.get_mut(&self.conversation) {
                for link in links {
                    if live.links.len() >= MAX_LINKS {
                        break;
                    }
                    live.links.insert(link);
                }
            }
        }
        self.with_data(|data| {
            data.reads.push(ReadRecord {
                tab: tab.to_owned(),
                epoch: facts.epoch,
                readout,
                page,
            })
        });
        self.handle(tab);
        Ok(index)
    }

    fn page_block(&self, index: usize) -> String {
        self.with_data(|data| {
            let read = data.reads.get(index)?;
            let letters = refs::letters(u16::try_from(index).ok()?)?;
            Some(prompt::page_block(&letters, &read.page))
        })
        .flatten()
        .unwrap_or_default()
    }

    async fn outline(&self, index: usize) -> Option<Outline> {
        self.browser_admission().ok()?;
        if let Some(outline) = self
            .with_data(|data| data.outlines.get(&index).cloned())
            .flatten()
        {
            return Some(outline);
        }
        let read = self
            .with_data(|data| data.reads.get(index).cloned())
            .flatten()?;
        self.browser_admission().ok()?;
        let outline = tokio::time::timeout(
            OUTLINE_TIMEOUT,
            self.service.ports().page.outline(&read.tab, read.epoch),
        )
        .await
        .ok()?
        .ok()
        .map(Outline::admit)?;
        self.browser_admission().ok()?;
        self.with_data(|data| data.outlines.insert(index, outline.clone()));
        Some(outline)
    }

    // ------------------------------------------------------------- the model

    /// Reads an attached page; the first one is where the reply works.
    async fn read_first(&self, tab: &str) -> Result<usize, PageAssistFaultV1> {
        self.with_data(|data| {
            data.working.get_or_insert_with(|| tab.to_owned());
        });
        self.read_tab(tab).await
    }

    async fn context(&self) -> Vec<String> {
        let mut parts = Vec::new();
        let mut first_read = None;
        for attachment in &self.attachments {
            match attachment {
                Admitted::Page { tab } => match self.read_first(tab).await {
                    Ok(index) => {
                        first_read.get_or_insert(index);
                        parts.push(self.page_block(index));
                    }
                    Err(_) => {
                        let host = self
                            .service
                            .ports()
                            .page
                            .facts(tab)
                            .and_then(|facts| facts.url)
                            .map(|url| host_of(&url))
                            .unwrap_or_default();
                        parts.push(format!("(The page {host} could not be read just now.)"));
                    }
                },
                Admitted::Selection { tab, text } => {
                    let letters = self
                        .with_data(|data| data.reads.iter().position(|read| &read.tab == tab))
                        .flatten()
                        .and_then(|index| refs::letters(u16::try_from(index).ok()?))
                        .unwrap_or_else(|| "?".to_owned());
                    parts.push(prompt::selection_block(&letters, text));
                }
                Admitted::Element {
                    host, label, pick, ..
                } => {
                    parts.push(prompt::element_block(
                        host,
                        label,
                        pick.selector.as_deref(),
                        &pick.text,
                    ));
                }
            }
        }
        let wants_style = self.suggestion.is_some_and(suggest::restyles);
        if let (true, Some(index)) = (wants_style, first_read) {
            if let Some(outline) = self.outline(index).await {
                parts.push(prompt::outline_block(&outline));
            }
        }
        parts.push(
            if self.native_budget.is_some() && !self.words.trim().is_empty() {
                self.words.clone()
            } else {
                prompt::request(&self.words, self.suggestion)
            },
        );
        parts
    }

    fn history(&self) -> Vec<WorkModelMessage> {
        let messages = {
            let state = self.service.state();
            state
                .live
                .get(&self.conversation)
                .map(|live| live.conversation.messages().to_vec())
                .unwrap_or_default()
        };
        let mut model = Vec::new();
        for exchange in prompt::history(&messages) {
            model.push(WorkModelMessage::User(vec![WorkModelPart::Text(
                exchange.person,
            )]));
            model.push(WorkModelMessage::Assistant(vec![WorkModelPart::Text(
                exchange.reply,
            )]));
        }
        model
    }

    async fn call(
        &self,
        model: &ResolvedModel,
        request: WorkModelRequest,
        text: &mut Text,
        hosted: bool,
    ) -> Result<zephium_core::work::model::WorkModelOutcome, End> {
        let (sender, mut receiver) = mpsc::unbounded_channel::<String>();
        let events = move |event: WorkModelEvent| {
            if let WorkModelEvent::Text(chunk) = event {
                let _ = sender.send(chunk);
            }
        };
        let call = tokio::time::timeout(CALL_TIMEOUT, model.client.call(request, &events));
        tokio::pin!(call);
        let outcome = loop {
            let wait = text
                .pacer
                .deadline()
                .map(|due| Duration::from_millis(due.saturating_sub(self.now())));
            tokio::select! {
                biased;
                Some(chunk) = receiver.recv() => self.take(text, &chunk),
                _ = tokio::time::sleep(wait.unwrap_or_default()), if wait.is_some() => {
                    let due = text.pacer.poll(self.now());
                    self.deliver(text, due);
                }
                outcome = &mut call => break outcome,
            }
        };
        while let Ok(chunk) = receiver.try_recv() {
            self.take(text, &chunk);
        }
        match outcome {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(error)) => Err(End::Failed(fault::model_fault(error, hosted))),
            Err(_)
                if model.entry.model.provider
                    == zephium_core::work::model::WorkModelProvider::OpenAi
                    && model.entry.id.starts_with("chatgpt/") =>
            {
                Err(End::Failed(PageAssistFaultV1::ChatGpt {
                    fault: zephium_core::work::model::ChatGptModelFault::Diagnostic {
                        stage: zephium_core::work::model::ChatGptFailureStage::Send,
                        status: None,
                        issue: zephium_core::work::model::ChatGptFailureIssue::Timeout,
                        field: None,
                    },
                }))
            }
            Err(_) => Err(End::Failed(PageAssistFaultV1::Model {
                fault: WorkModelFaultV1::ProviderDown,
                retry_after_ms: None,
            })),
        }
    }
}

fn add_usage(total: &mut WorkModelUsage, usage: &WorkModelUsage) {
    if !total.is_reported() || !usage.is_reported() {
        total.status = zephium_core::work::model::WorkModelUsageStatus::Unavailable;
        total.cost_micros = None;
        return;
    }
    total.input_tokens += usage.input_tokens;
    total.cached_input_tokens += usage.cached_input_tokens;
    total.cache_write_input_tokens += usage.cache_write_input_tokens;
    total.output_tokens += usage.output_tokens;
    total.reasoning_tokens += usage.reasoning_tokens;
    total.cost_micros = match (total.cost_micros, usage.cost_micros) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
}

/// Only native turns need an eager handoff followed by an independently
/// cancellable waiter. This guard never releases actor-owned turn capacity.
struct NativeTaskGuard {
    service: PageAssist,
    conversation: String,
    reply: u32,
    cancel: Arc<Cancel>,
    callback_state: Arc<std::sync::Mutex<NativeCallbackState>>,
    leased: Arc<AtomicBool>,
    finished: bool,
}
impl Drop for NativeTaskGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.callback_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .open = false;
        self.cancel.cancel(CancelReason::Gone);
        self.leased.store(false, Ordering::Release);
        let event = {
            let mut state = self.service.state();
            if state.running.get(&self.conversation).is_none_or(|running| {
                running.reply != self.reply || !Arc::ptr_eq(&running.cancel, &self.cancel)
            }) {
                return;
            }
            state.running.remove(&self.conversation);
            state.live.get_mut(&self.conversation).and_then(|live| {
                let event = live.conversation.apply(
                    self.reply,
                    PageAssistEventV1::Failed {
                        fault: PageAssistFaultV1::NativeConversation {
                            fault: PageAssistNativeFaultV1::OutcomeUnknown,
                        },
                    },
                );
                if event.is_some() {
                    live.revision = Arc::new(());
                }
                event
            })
        };
        self.service.emit_all(event);
        self.service.persist(&self.conversation);
    }
}
struct NativeTask {
    // Drop callback/lifetime guard before the owned waiter; its cancellation
    // guard then signals the native actor, without freeing its turn slot.
    guard: NativeTaskGuard,
    inner: super::TurnTask,
}
impl std::future::Future for NativeTask {
    type Output = ();
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let this = self.get_mut();
        let result = this.inner.as_mut().poll(cx);
        if result.is_ready() {
            this.guard.finished = true;
        }
        result
    }
}
pub(super) fn task(reply: Reply) -> super::TurnTask {
    if reply.native_budget.is_some() {
        return browser::task(reply);
    }
    let callback_state = reply
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|turn| turn.callback_state.clone());
    match callback_state {
        Some(callback_state) => {
            let guard = NativeTaskGuard {
                service: reply.service.clone(),
                conversation: reply.conversation.clone(),
                reply: reply.reply,
                cancel: reply.cancel.clone(),
                callback_state,
                leased: reply.leased.clone(),
                finished: false,
            };
            Box::pin(NativeTask {
                guard,
                inner: Box::pin(run(reply)),
            })
        }
        None => Box::pin(run(reply)),
    }
}

/// Runs one admitted reply to its end.
async fn run(reply: Reply) {
    let end = if reply.native_budget.is_some() {
        // The browser owner drains an entered effect even after UI cancellation.
        body(&reply).await
    } else {
        tokio::select! {
            biased;
            reason = reply.cancel.cancelled() => End::Cancelled(reason),
            end = body(&reply) => end,
        }
    };
    reply.leased.store(false, Ordering::Release);
    reply.unglow();
    reply.note(format_args!(
        "end {}",
        match &end {
            End::NativeProjected => "native-projected".to_owned(),
            End::Done(_) => "done".to_owned(),
            End::Failed(fault) => format!("failed fault={fault:?}"),
            End::Cancelled(reason) => format!("cancelled reason={reason:?}"),
        }
    ));
    match end {
        End::NativeProjected => {}
        End::Done(cost) => {
            reply.apply(PageAssistEventV1::Done { cost });
        }
        End::Failed(fault) => {
            reply.apply(PageAssistEventV1::Failed { fault });
        }
        End::Cancelled(CancelReason::Stopped) => {
            reply.apply(PageAssistEventV1::Cancelled);
        }
        End::Cancelled(CancelReason::Gone) => {}
    }
    {
        let mut state = reply.service.state();
        if state
            .running
            .get(&reply.conversation)
            .is_some_and(|running| {
                running.reply == reply.reply
                    && (!matches!(reply.backend, PageBackendChoice::NativeConversation(_))
                        || Arc::ptr_eq(&running.cancel, &reply.cancel))
            })
        {
            state.running.remove(&reply.conversation);
        }
    }
    reply.service.persist(&reply.conversation);
}

fn native_fault(fault: PageAssistNativeFaultV1) -> End {
    End::Failed(PageAssistFaultV1::NativeConversation { fault })
}

/// Fences callbacks even if a broken adapter retains them after the future ends.
struct NativeCallbackState {
    open: bool,
    fault: Option<PageAssistNativeFaultV1>,
}
struct NativeCallbackLease(Arc<std::sync::Mutex<NativeCallbackState>>);
impl Drop for NativeCallbackLease {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).open = false;
    }
}

pub(super) enum NativeTurnFuture {
    Text(
        super::port::BoxFuture<
            'static,
            Result<super::port::NativeConversationOutcome, PageAssistNativeFaultV1>,
        >,
    ),
    Browser(
        super::port::BoxFuture<
            'static,
            Result<super::port::NativeBrowserCompletion, PageAssistNativeFaultV1>,
        >,
    ),
}

pub(super) struct NativeTurn {
    future: NativeTurnFuture,
    callback_state: Arc<std::sync::Mutex<NativeCallbackState>>,
    _lease: NativeCallbackLease,
    deadline: tokio::time::Instant,
    browser: Option<browser::Turn>,
}
/// Synchronous local handoff before a caller can drop an unpolled TurnTask.
/// Native callbacks may arrive immediately; Page's running identity is installed.
pub(super) fn commit_native(reply: &Reply, native: Box<dyn NativeConversationReservation>) {
    debug_assert!(reply.suggestion.is_none() && reply.attachments.is_empty());
    let active = Arc::new(std::sync::Mutex::new(NativeCallbackState {
        open: true,
        fault: None,
    }));
    let callback_state = active.clone();
    let _lease = NativeCallbackLease(active.clone());
    let service = reply.service.clone();
    let conversation = reply.conversation.clone();
    let reply_id = reply.reply;
    let cancel = reply.cancel.clone();
    let events: super::port::NativeConversationEvents = Arc::new(move |event| {
        let mut active = active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(fault) = active.fault {
            return Err(fault);
        }
        if !active.open || cancel.reason().is_some() {
            return Err(PageAssistNativeFaultV1::Cancelled);
        }
        let NativeConversationEvent::TextSnapshot { text } = event;
        if text.len() > zephium_page_assist::conversation::MAX_TEXT_BYTES {
            active.fault = Some(PageAssistNativeFaultV1::Limit);
            return Err(PageAssistNativeFaultV1::Limit);
        }
        let event = {
            let mut state = service.state();
            if state.running.get(&conversation).is_none_or(|running| {
                running.reply != reply_id || !Arc::ptr_eq(&running.cancel, &cancel)
            }) {
                active.fault = Some(PageAssistNativeFaultV1::ScopeChanged);
                return Err(PageAssistNativeFaultV1::ScopeChanged);
            }
            state
                .live
                .get_mut(&conversation)
                .and_then(|live| {
                    let event = live
                        .conversation
                        .apply(reply_id, PageAssistEventV1::TextSnapshot { block: 0, text });
                    if event.is_some() {
                        live.revision = Arc::new(());
                    }
                    event
                })
                .ok_or(PageAssistNativeFaultV1::ScopeChanged)?
        };
        service.emit_all([event]);
        Ok(())
    });
    let future = native.commit(reply.reply, reply.cancel.native.clone(), events);
    *reply.native.lock().unwrap_or_else(|p| p.into_inner()) = Some(NativeTurn {
        future: NativeTurnFuture::Text(future),
        callback_state,
        _lease,
        deadline: tokio::time::Instant::now() + CALL_TIMEOUT,
        browser: None,
    });
}

async fn native_body(reply: &Reply, native: NativeTurn) -> End {
    let NativeTurn {
        future,
        callback_state,
        _lease,
        deadline,
        browser: _,
    } = native;
    let NativeTurnFuture::Text(future) = future else {
        return native_fault(PageAssistNativeFaultV1::Protocol);
    };
    let result = tokio::time::timeout_at(deadline, future).await;
    let callback_fault = {
        let mut state = callback_state.lock().unwrap_or_else(|p| p.into_inner());
        // Join callbacks before projecting the authoritative terminal text.
        // A retained emitter cannot race a late delta over that final snapshot.
        state.open = false;
        state.fault
    };
    if let Some(fault) = callback_fault {
        return native_fault(fault);
    }
    let outcome = match result {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(fault)) => return native_fault(fault),
        Err(_) => return native_fault(PageAssistNativeFaultV1::Timeout),
    };
    match outcome.status {
        NativeConversationStatus::Completed | NativeConversationStatus::HistoryUncertain => {
            let text = outcome.final_response.unwrap_or_default();
            if text.len() > zephium_page_assist::conversation::MAX_TEXT_BYTES {
                return native_fault(PageAssistNativeFaultV1::Limit);
            }
            if !reply.apply(PageAssistEventV1::TextSnapshot { block: 0, text }) {
                return native_fault(PageAssistNativeFaultV1::ScopeChanged);
            }
            if outcome.status == NativeConversationStatus::HistoryUncertain {
                native_fault(PageAssistNativeFaultV1::HistoryUncertain)
            } else {
                End::Done(None)
            }
        }
        NativeConversationStatus::NeverStarted => {
            if outcome.final_response.is_some() {
                native_fault(PageAssistNativeFaultV1::Protocol)
            } else {
                native_fault(PageAssistNativeFaultV1::NotSent)
            }
        }
        NativeConversationStatus::Interrupted => End::Cancelled(CancelReason::Stopped),
        NativeConversationStatus::Failed => native_fault(PageAssistNativeFaultV1::Upstream),
        NativeConversationStatus::Unknown => native_fault(PageAssistNativeFaultV1::OutcomeUnknown),
    }
}

async fn body(reply: &Reply) -> End {
    let ports = reply.service.ports();
    if matches!(&reply.backend, PageBackendChoice::NativeConversation(_)) {
        let reservation = reply
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        return match reservation {
            Some(reservation) if reservation.browser.is_some() => {
                browser::body(reply, reservation).await
            }
            Some(reservation) => native_body(reply, reservation).await,
            None => native_fault(PageAssistNativeFaultV1::ScopeChanged),
        };
    }
    let source = WorkCallSource::Page {
        conversation: reply.conversation.clone(),
        reply: reply.reply,
    };
    reply.phase(Some(PageAssistPhaseV1::Thinking));
    let backend = match ports.models.resolve(&reply.profile, source).await {
        Ok(backend) => backend,
        Err(PageBackendError::Busy) => return End::Failed(PageAssistFaultV1::Busy),
        Err(PageBackendError::Model(error)) => {
            return End::Failed(fault::model_fault(error, false));
        }
        Err(PageBackendError::NativeConversation(fault)) => return native_fault(fault),
    };
    let model = match (&reply.backend, backend) {
        (PageBackendChoice::Model(_), ResolvedPageBackend::Model(model)) => model,
        _ => return native_fault(PageAssistNativeFaultV1::ScopeChanged),
    };
    let hosted = model.entry.model.provider == zephium_core::work::model::WorkModelProvider::Cloud;
    reply.note(format_args!(
        "start model={} effort={:?} tools={} attachments={} suggestion={:?}",
        model.entry.id,
        model.effort,
        model.entry.supports.tools,
        reply.attachments.len(),
        reply.suggestion
    ));
    let parts = reply.context().await;
    let mut messages = reply.history();
    messages.push(WorkModelMessage::User(
        parts.into_iter().map(WorkModelPart::Text).collect(),
    ));
    let system = vec![WorkModelSystemBlock {
        text: prompt::SYSTEM.to_owned(),
        cache: true,
    }];
    let tools = if model.entry.supports.tools {
        tools::definitions()
    } else {
        Vec::new()
    };
    let cache_key = work_model_cache_key(&system, &tools);
    let mut text = Text::default();
    let mut usage = WorkModelUsage::default();
    let mut ran = 0usize;
    let mut calls_left = MAX_CALLS;
    while calls_left > 0 {
        if text.block.is_none() {
            reply.phase(Some(PageAssistPhaseV1::Thinking));
        }
        let request = WorkModelRequest {
            image_fence: None,
            cache_key: Some(cache_key.clone()),
            model: model.entry.model.clone(),
            system: system.clone(),
            tools: tools.clone(),
            messages: messages.clone(),
            max_output_tokens: prompt::MAX_OUTPUT_TOKENS.min(model.entry.max_output.max(8_192)),
            reasoning: model.effort.filter(|_| model.entry.supports.reasoning),
            native_search: false,
            parallel_tools: false,
            tool_choice: WorkModelToolChoice::Auto,
            volatile_tail: 0,
        };
        let outcome = match reply.call(&model, request, &mut text, hosted).await {
            Ok(outcome) => outcome,
            Err(end) => {
                reply.flush(&mut text);
                return end;
            }
        };
        add_usage(&mut usage, &outcome.usage);
        if outcome.usage.is_reported() {
            reply.note(format_args!(
                "call stop={:?} in={} cached={} out={} reasoning={} text_blocks={}",
                outcome.stop,
                outcome.usage.input_tokens,
                outcome.usage.cached_input_tokens,
                outcome.usage.output_tokens,
                outcome.usage.reasoning_tokens,
                reply.next_block()
            ));
        } else {
            reply.note(format_args!(
                "call stop={:?} usage=unavailable text_blocks={}",
                outcome.stop,
                reply.next_block()
            ));
        }
        let exhausted = outcome.stop == WorkModelStop::MaxTokens;
        let calls: Vec<_> = outcome
            .assistant
            .iter()
            .filter_map(|part| match part {
                WorkModelPart::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();
        // Usage completeness does not authorize tools. The original Page loop's
        // call/tool/time bounds and each tool's browser authority remain decisive.
        messages.push(WorkModelMessage::Assistant(outcome.assistant));
        if exhausted && calls.is_empty() && reply.next_block() == 0 && text.block.is_none() {
            reply.flush(&mut text);
            return End::Failed(PageAssistFaultV1::TooLong);
        }
        if outcome.stop != WorkModelStop::ToolUse || calls.is_empty() {
            break;
        }
        reply.flush(&mut text);
        let mut results = Vec::with_capacity(calls.len());
        let mut stop = false;
        for call in calls {
            ran += 1;
            let outcome = if ran > MAX_TOOLS {
                Err("Too many tool calls in one reply; answer with what you have.".to_owned())
            } else {
                match tools::parse(&call.name, &call.arguments) {
                    Some(parsed) => {
                        stop |= matches!(parsed, Call::HandOff { .. });
                        match run_tool(reply, &mut text, parsed).await {
                            Ok(outcome) => outcome,
                            Err(end) => return end,
                        }
                    }
                    None => Err(format!(
                        "{} was called with arguments it does not take.",
                        call.name
                    )),
                }
            };
            let (content, is_error) = match outcome {
                Ok(content) => (content, false),
                Err(content) => (content, true),
            };
            reply.note(format_args!(
                "tool name={} ok={} result_bytes={}",
                tools::known(&call.name),
                !is_error,
                content.len()
            ));
            results.push(WorkModelToolResult {
                call: call.id,
                content,
                is_error,
            });
        }
        messages.push(WorkModelMessage::ToolResults(results));
        if stop {
            break;
        }
        calls_left -= 1;
        if calls_left == 1 {
            messages.push(WorkModelMessage::User(vec![WorkModelPart::Text(
                "Stop using tools now. In two or three sentences tell the person what you did and what is left.".to_owned(),
            )]));
        }
    }
    reply.flush(&mut text);
    reply.phase(None);
    if reply.next_block() == 0 {
        return End::Failed(PageAssistFaultV1::NoAnswer);
    }
    End::Done(
        usage
            .is_reported()
            .then(|| fault::cost(&model.entry, &usage)),
    )
}

async fn run_tool(reply: &Reply, text: &mut Text, call: Call) -> Result<ToolOutcome, End> {
    Ok(match call {
        Call::ListTabs => Ok(list_tabs(reply)),
        Call::ReadPage { tab } => read_page(reply, &tab).await,
        Call::ShowItems { items } => show_items(reply, text, items).await,
        Call::OpenTabs { targets } => open_tabs(reply, text, targets).await,
        Call::GoTo { target, new_tab } => go_to(reply, text, &target, new_tab).await,
        Call::Act { steps } => return act(reply, text, steps).await,
        Call::InspectStyle { page } => inspect_style(reply, &page).await,
        Call::Restyle { page, summary, css } => restyle(reply, text, &page, summary, &css).await,
        Call::HandOff {
            title,
            summary,
            bring,
        } => hand_off(reply, text, title, summary, bring).await,
    })
}

fn list_tabs(reply: &Reply) -> String {
    let tabs = reply
        .service
        .ports()
        .tabs
        .list(&reply.profile, &reply.space);
    reply.admit_links(tabs.iter().filter_map(|tab| tab.url.clone()));
    let lines: Vec<String> = tabs
        .iter()
        .take(60)
        .map(|tab| {
            let host = tab.url.as_deref().map(host_of).unwrap_or_default();
            let title = text::bounded(&text::clean(&tab.title), 90).replace('"', "'");
            format!(
                "{} · {} · \"{}\"",
                reply.handle(&tab.id),
                host,
                prompt::fence(&title)
            )
        })
        .collect();
    if lines.is_empty() {
        "No tabs are open in this Space.".to_owned()
    } else {
        lines.join("\n")
    }
}

async fn read_page(reply: &Reply, handle: &str) -> ToolOutcome {
    let tab = reply
        .tab_of(handle)
        .ok_or_else(|| format!("{handle} is not a tab handle from list_tabs."))?;
    match reply.read_tab(&tab).await {
        Ok(index) => Ok(reply.page_block(index)),
        Err(PageAssistFaultV1::Sensitive) => {
            Err("That tab is a sensitive site and is never read.".to_owned())
        }
        Err(PageAssistFaultV1::TooLong) => {
            Err("No more pages can be read in this reply.".to_owned())
        }
        Err(_) => Err(
            "That tab is asleep or still loading. go_to its address from list_tabs to open it fresh."
                .to_owned(),
        ),
    }
}

async fn show_items(reply: &Reply, text: &mut Text, calls: Vec<ItemCall>) -> ToolOutcome {
    let mut items = Vec::with_capacity(calls.len());
    let mut marks = Vec::new();
    let mut page: Option<ReadRecord> = None;
    for (index, call) in calls.into_iter().enumerate() {
        let n = u8::try_from(index + 1).unwrap_or(u8::MAX);
        let reference = call.reference.as_deref().and_then(refs::parse);
        let record = reference.and_then(|reference| reply.record_of(reference));
        let fragment = reference
            .zip(record.as_ref())
            .and_then(|(reference, record)| record.page.fragments.get(&reference.passage).cloned());
        if let (Some(reference), Some(record), Some(_)) = (reference, &record, &fragment) {
            let same = page.as_ref().is_none_or(|page| {
                page.page.payload == record.page.payload && page.tab == record.tab
            });
            if same {
                page.get_or_insert_with(|| record.clone());
                marks.push((n, reference.passage));
            }
        }
        items.push(PageAssistItemV1 {
            n,
            title: call.title,
            meta: call.meta,
            url: fragment.and_then(|fragment| fragment.href),
            marked: false,
        });
    }
    let tab = page
        .as_ref()
        .map(|page| page.tab.clone())
        .unwrap_or_default();
    let block = reply.add_block(
        text,
        PageAssistBlockV1::Items {
            tab: tab.clone(),
            items: items.clone(),
        },
    );
    let Some(page) = page else {
        return Ok(format!(
            "Showed {} items; none could be marked on a page.",
            items.len()
        ));
    };
    reply.browser_effect()?;
    let placed = reply
        .service
        .ports()
        .page
        .marks(&page.tab, page.epoch, page.readout.read, marks)
        .await;
    let previous = {
        let mut state = reply.service.state();
        state.marks.insert(
            page.tab.clone(),
            (reply.conversation.clone(), reply.reply, block),
        )
    };
    if let Some((conversation, earlier, earlier_block)) = previous {
        if (conversation.as_str(), earlier, earlier_block)
            != (reply.conversation.as_str(), reply.reply, block)
        {
            reply.service.unmark(&conversation, earlier, earlier_block);
        }
    }
    for item in &mut items {
        item.marked = placed.contains(&item.n);
    }
    reply.set_block(
        block,
        PageAssistBlockV1::Items {
            tab,
            items: items.clone(),
        },
    );
    Ok(format!(
        "Showed {} items as cards; {} marked on the page. Don't repeat them in text.",
        items.len(),
        placed.len()
    ))
}

async fn open_tabs(reply: &Reply, text: &mut Text, targets: Vec<String>) -> ToolOutcome {
    let mut urls: Vec<String> = Vec::new();
    let mut refused = Vec::new();
    let links = {
        let state = reply.service.state();
        state
            .live
            .get(&reply.conversation)
            .map(|live| live.links.clone())
            .unwrap_or_default()
    };
    for target in targets {
        let url = match refs::parse(&target) {
            Some(reference) => reply
                .record_of(reference)
                .and_then(|record| record.page.fragments.get(&reference.passage)?.href.clone()),
            None => Some(target.clone()).filter(|url| links.contains(url)),
        };
        match url {
            Some(url) if !urls.contains(&url) => urls.push(url),
            Some(_) => {}
            None => refused.push(target),
        }
        if urls.len() == tools::MAX_OPEN {
            break;
        }
    }
    if urls.is_empty() {
        return Err(
            "None of those targets is a link from a page you read or an address the person wrote."
                .to_owned(),
        );
    }
    reply.phase(Some(PageAssistPhaseV1::Opening));
    let mut rows: Vec<PageAssistOpenedV1> = urls
        .iter()
        .enumerate()
        .map(|(index, url)| PageAssistOpenedV1 {
            index: u8::try_from(index).unwrap_or(u8::MAX),
            host: host_of(url),
            url: url.clone(),
            tab: None,
            state: PageAssistOpenStateV1::Queued,
        })
        .collect();
    let block = reply.add_block(text, PageAssistBlockV1::Opened { tabs: rows.clone() });
    let ports = reply.service.ports().clone();
    let mut after = reply
        .with_data(|data| data.handles.first().cloned())
        .flatten();
    for index in 0..rows.len() {
        reply.browser_effect()?;
        rows[index].state = PageAssistOpenStateV1::Opening;
        reply.set_block(block, PageAssistBlockV1::Opened { tabs: rows.clone() });
        let opened = ports
            .tabs
            .open(
                &reply.profile,
                &reply.space,
                &rows[index].url,
                after.as_deref(),
                false,
            )
            .await;
        match opened {
            Some(tab) => {
                rows[index].tab = Some(tab.clone());
                reply.set_block(block, PageAssistBlockV1::Opened { tabs: rows.clone() });
                settled(&ports, &tab).await;
                rows[index].state = PageAssistOpenStateV1::Opened;
                after = Some(tab);
            }
            None => rows[index].state = PageAssistOpenStateV1::Failed,
        }
        reply.set_block(block, PageAssistBlockV1::Opened { tabs: rows.clone() });
    }
    reply.phase(Some(PageAssistPhaseV1::Thinking));
    let opened: Vec<&str> = rows
        .iter()
        .filter(|row| row.state == PageAssistOpenStateV1::Opened)
        .map(|row| row.host.as_str())
        .collect();
    let mut summary = format!(
        "Opened {} of {} in background tabs: {}.",
        opened.len(),
        rows.len(),
        opened.join(", ")
    );
    if !refused.is_empty() {
        summary.push_str(&format!(
            " Not opened, not from a page you read: {}.",
            refused.join(", ")
        ));
    }
    Ok(summary)
}

/// Waits for a new tab's first load, bounded.
async fn settled(ports: &super::Ports, tab: &str) {
    let started = tokio::time::Instant::now();
    while started.elapsed() < OPEN_TIMEOUT {
        match ports.page.facts(tab) {
            Some(facts) if !facts.loading => break,
            None if started.elapsed() > UNLISTED_TIMEOUT => break,
            _ => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }
}

/// One page in front, read for the model: a read link, an address the
/// person wrote or a listed tab's, or a search on the person's own engine.
async fn go_to(reply: &Reply, text: &mut Text, target: &str, new_tab: bool) -> ToolOutcome {
    let ports = reply.service.ports().clone();
    let target = target.trim();
    let url = if let Some(query) = target
        .strip_prefix("search:")
        .map(str::trim)
        .filter(|query| !query.is_empty())
    {
        ports
            .tabs
            .search_url(&reply.profile, query)
            .ok_or_else(|| "That search cannot be made.".to_owned())?
    } else if let Some(reference) = refs::parse(target) {
        reply
            .record_of(reference)
            .and_then(|record| record.page.fragments.get(&reference.passage)?.href.clone())
            .ok_or_else(|| format!("{target} is not a link of a page you read."))?
    } else {
        let known = {
            let state = reply.service.state();
            state
                .live
                .get(&reply.conversation)
                .is_some_and(|live| live.links.contains(target))
        };
        if !known {
            return Err("Not an address the person wrote, a link you read or a listed tab. Use `search: words` instead.".to_owned());
        }
        target.to_owned()
    };
    let working = reply.with_data(|data| data.working.clone()).flatten();
    match working.filter(|_| !new_tab) {
        Some(tab) => move_tab(reply, text, &ports, &tab, &url).await,
        None => open_in_front(reply, text, &ports, &url).await,
    }
}

/// Follows `url` in the tab the reply works in, as a step on that page.
async fn move_tab(
    reply: &Reply,
    text: &mut Text,
    ports: &super::Ports,
    tab: &str,
    url: &str,
) -> ToolOutcome {
    let host = host_of(url);
    let before = ports.page.facts(tab).map(|facts| facts.epoch);
    let mut row = PageAssistStepV1 {
        index: 0,
        label: format!("Go to {host}"),
        category: None,
        state: PageAssistStepStateV1::Running,
        reason: None,
    };
    reply.phase(Some(PageAssistPhaseV1::Acting));
    reply.glow(tab, Glow::Working);
    let block = reply.add_block(
        text,
        PageAssistBlockV1::Steps {
            steps: vec![row.clone()],
        },
    );
    reply.browser_effect()?;
    if !ports.tabs.navigate(&reply.profile, tab, url) {
        row.state = PageAssistStepStateV1::Failed;
        row.reason = Some(PageAssistStepReasonV1::Refused);
        reply.set_block(block, PageAssistBlockV1::Steps { steps: vec![row] });
        return Err(format!(
            "{host} could not be opened in this tab; try new_tab."
        ));
    }
    let started = tokio::time::Instant::now();
    while started.elapsed() < OPEN_TIMEOUT {
        match ports.page.facts(tab) {
            Some(facts) if Some(facts.epoch) != before && !facts.loading => break,
            None => break,
            _ => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }
    row.state = PageAssistStepStateV1::Done;
    reply.set_block(block, PageAssistBlockV1::Steps { steps: vec![row] });
    read_after_open(reply, tab, &host, "this tab").await
}

/// Opens `url` in a new tab in front; the reply works there from now on.
async fn open_in_front(
    reply: &Reply,
    text: &mut Text,
    ports: &super::Ports,
    url: &str,
) -> ToolOutcome {
    reply.phase(Some(PageAssistPhaseV1::Opening));
    let after = reply
        .with_data(|data| {
            data.working
                .clone()
                .or_else(|| data.handles.first().cloned())
        })
        .flatten();
    let host = host_of(url);
    let mut row = PageAssistOpenedV1 {
        index: 0,
        host: host.clone(),
        url: url.to_owned(),
        tab: None,
        state: PageAssistOpenStateV1::Opening,
    };
    let block = reply.add_block(
        text,
        PageAssistBlockV1::Opened {
            tabs: vec![row.clone()],
        },
    );
    reply.browser_effect()?;
    let Some(tab) = ports
        .tabs
        .open(&reply.profile, &reply.space, url, after.as_deref(), true)
        .await
    else {
        row.state = PageAssistOpenStateV1::Failed;
        reply.set_block(block, PageAssistBlockV1::Opened { tabs: vec![row] });
        return Err(format!("{host} could not be opened."));
    };
    row.tab = Some(tab.clone());
    reply.set_block(
        block,
        PageAssistBlockV1::Opened {
            tabs: vec![row.clone()],
        },
    );
    settled(ports, &tab).await;
    row.state = PageAssistOpenStateV1::Opened;
    reply.set_block(block, PageAssistBlockV1::Opened { tabs: vec![row] });
    reply.with_data(|data| data.working = Some(tab.clone()));
    read_after_open(reply, &tab, &host, "a new tab in front").await
}

async fn read_after_open(reply: &Reply, tab: &str, host: &str, place: &str) -> ToolOutcome {
    let handle = reply.handle(tab);
    let mut read = reply.read_tab(tab).await;
    if matches!(read, Err(PageAssistFaultV1::Loading)) {
        tokio::time::sleep(Duration::from_millis(800)).await;
        read = reply.read_tab(tab).await;
    }
    reply.phase(Some(PageAssistPhaseV1::Thinking));
    match read {
        Ok(index) => Ok(format!(
            "Opened {host} in {place} ({handle}).\n{}",
            reply.page_block(index)
        )),
        Err(PageAssistFaultV1::Sensitive) => Ok(format!(
            "Opened {host} in {place} ({handle}). It is a sensitive site and is never read; the person continues there."
        )),
        Err(_) => Ok(format!(
            "Opened {host} in {place} ({handle}), but it cannot be read yet; try read_page {handle} once."
        )),
    }
}

async fn inspect_style(reply: &Reply, page: &str) -> ToolOutcome {
    let (index, _) = reply
        .record(page)
        .ok_or_else(|| format!("{page} is not a page read in this reply."))?;
    match reply.outline(index).await {
        Some(outline) => Ok(prompt::outline_block(&outline)),
        None => Ok("No outline is available; use plain tags.".to_owned()),
    }
}

async fn restyle(
    reply: &Reply,
    text: &mut Text,
    page: &str,
    summary: String,
    sheet: &str,
) -> ToolOutcome {
    let (index, record) = reply
        .record(page)
        .ok_or_else(|| format!("{page} is not a page read in this reply."))?;
    let site = record.page.site.clone();
    let summary = text::bounded(&summary, PAGE_ASSIST_MAX_EXCERPT_CHARS);
    let proposal = |state, rules, id| PageAssistBlockV1::Style {
        style: PageStyleProposalV1 {
            id,
            summary: summary.clone(),
            site: site.clone(),
            rules,
            state,
        },
    };
    if !reply.service.site_has_room(&reply.profile, &site, 0) {
        reply.add_block(text, proposal(PageStyleStateV1::Full, 0, None));
        return Ok("This site already holds as many styles as it can; the person can remove one in Settings.".to_owned());
    }
    reply.phase(Some(PageAssistPhaseV1::Styling));
    let outline = reply.outline(index).await;
    // A returned outline cannot create a new actionable style proposal after
    // the original turn closed this Page action admission.
    reply.browser_effect()?;
    let validated = match css::validate(sheet) {
        Ok(validated) => validated,
        Err(rejection) => {
            reply.note(format_args!(
                "restyle rejected={rejection:?} bytes={}",
                sheet.len()
            ));
            return Err(format!(
                "The stylesheet was refused ({rejection:?}). Write it again within the rules."
            ));
        }
    };
    let Some(guarded) = guard::guard(&validated, outline.as_ref()) else {
        return Err(
            "Nothing in the stylesheet passed the rules. Write it again within the rules."
                .to_owned(),
        );
    };
    if !reply
        .service
        .site_has_room(&reply.profile, &site, guarded.css.len())
    {
        reply.add_block(text, proposal(PageStyleStateV1::Full, guarded.rules, None));
        return Ok("The stylesheet does not fit beside this site's other styles.".to_owned());
    }
    let rules = guarded.rules;
    let block = reply.add_block(text, proposal(PageStyleStateV1::Proposed, rules, None));
    reply.with_data(|data| {
        data.styles.insert(
            block,
            PendingStyle {
                profile: reply.profile,
                site: site.clone(),
                summary: summary.clone(),
                css: guarded.css,
                rules,
            },
        )
    });
    reply.phase(Some(PageAssistPhaseV1::Thinking));
    Ok(format!(
        "Proposed “{summary}” for {site}: {rules} rules passed. The person applies it from the card."
    ))
}

fn transcript(reply: &Reply) -> String {
    let messages = {
        let state = reply.service.state();
        state
            .live
            .get(&reply.conversation)
            .map(|live| live.conversation.messages().to_vec())
            .unwrap_or_default()
    };
    let mut lines: Vec<String> = prompt::history(&messages)
        .into_iter()
        .map(|exchange| format!("Person: {}\nZephium: {}", exchange.person, exchange.reply))
        .collect();
    lines.push(format!(
        "Person: {}",
        prompt::request(&reply.words, reply.suggestion)
    ));
    let joined = lines.join("\n\n");
    let skip = joined.chars().count().saturating_sub(MAX_TRANSCRIPT_CHARS);
    joined.chars().skip(skip).collect()
}

async fn hand_off(
    reply: &Reply,
    text: &mut Text,
    title: String,
    summary: String,
    bring: BringRequest,
) -> ToolOutcome {
    reply.phase(Some(PageAssistPhaseV1::Preparing));
    let mut tabs: Vec<String> = bring
        .tabs
        .iter()
        .filter_map(|handle| reply.tab_of(handle))
        .collect();
    for attachment in &reply.attachments {
        if let Admitted::Page { tab } = attachment {
            if !tabs.contains(tab) {
                tabs.insert(0, tab.clone());
            }
        }
    }
    let request = BringRequest { tabs, ..bring };
    let brings = reply
        .service
        .ports()
        .work
        .prepare(&reply.profile, &reply.space, request)
        .await;
    let objective = format!(
        "{title}\n\n{summary}\n\nFrom the sidebar conversation:\n{}",
        transcript(reply)
    );
    let block = reply.add_block(
        text,
        PageAssistBlockV1::Handoff {
            handoff: PageAssistHandoffV1 {
                title: title.clone(),
                summary: summary.clone(),
                brings: brings.clone(),
                state: PageAssistHandoffStateV1::Offered,
            },
        },
    );
    reply.with_data(|data| {
        data.handoffs.insert(
            block,
            WorkStart {
                title,
                objective,
                brings,
            },
        )
    });
    Ok(
        "Offered to the person as a Work card. Stop here: write at most one short sentence."
            .to_owned(),
    )
}

// -------------------------------------------------------------------- steps

fn category(effect: StepEffect) -> Option<WorkConfirmCategoryV1> {
    match effect {
        StepEffect::Commit(category) => Some(category),
        StepEffect::Read | StepEffect::Draft => None,
    }
}

/// Refuses, from the step's own read, steps the engine must never run:
/// protected fields and links away from the site are refused, anything a
/// verb cannot use is unsuitable.
fn admissible(record: &ReadRecord, step: &StepRequest) -> Result<(), StepFailure> {
    let Some(target) = step.target else {
        return if step.verb == Verb::Scroll {
            Ok(())
        } else {
            Err(StepFailure::Unsuitable)
        };
    };
    let Some(passage) = record
        .readout
        .passages
        .iter()
        .find(|passage| passage.id == target)
    else {
        return Err(StepFailure::Unsuitable);
    };
    if matches!(passage.role, Role::Field(kind) if kind.protected()) {
        return Err(StepFailure::Refused);
    }
    if matches!(
        (step.verb, passage.role),
        (Verb::Open | Verb::Click, Role::Link { same_site: false })
    ) {
        return Err(StepFailure::Refused);
    }
    let fits = match (step.verb, passage.role) {
        (Verb::Fill, Role::Field(_)) => true,
        (Verb::Fill, _) => false,
        (Verb::Submit, Role::Field(_)) => true,
        (Verb::Submit, _) => false,
        (Verb::Open, Role::Link { same_site }) => same_site,
        (Verb::Open, _) => false,
        (Verb::Select, Role::Select) => true,
        (Verb::Select, _) => false,
        (Verb::Check | Verb::Uncheck, Role::Checkbox { .. }) => true,
        (Verb::Check | Verb::Uncheck, _) => false,
        (Verb::Click, Role::Link { same_site }) => same_site,
        (Verb::Click | Verb::Expand, Role::Button | Role::Checkbox { .. } | Role::Other) => true,
        (Verb::Click | Verb::Expand, _) => false,
        (Verb::Scroll, _) => true,
    };
    if fits {
        Ok(())
    } else {
        Err(StepFailure::Unsuitable)
    }
}

struct Plan {
    tab: String,
    epoch: u64,
    record: ReadRecord,
    requests: Vec<StepRequest>,
    rows: Vec<PageAssistStepV1>,
    /// The engine's label once a step resolved, to recognise it after a pause.
    resolved: Vec<Option<String>>,
    block: u16,
}

impl Plan {
    fn show(&self, reply: &Reply) {
        reply.set_block(
            self.block,
            PageAssistBlockV1::Steps {
                steps: self.rows.clone(),
            },
        );
    }

    fn fail(&mut self, reply: &Reply, index: usize, failure: StepFailure) {
        if let Some(row) = self.rows.get_mut(index) {
            row.state = PageAssistStepStateV1::Failed;
            row.reason = Some(match failure {
                StepFailure::Refused => PageAssistStepReasonV1::Refused,
                StepFailure::Unsuitable => PageAssistStepReasonV1::Unsuitable,
                StepFailure::PageChanged => PageAssistStepReasonV1::PageChanged,
                StepFailure::Failed => PageAssistStepReasonV1::NoEffect,
            });
        }
        self.skip_from(reply, index + 1);
    }

    fn mark(
        &mut self,
        reply: &Reply,
        index: usize,
        label: &str,
        effect: StepEffect,
        state: PageAssistStepStateV1,
    ) {
        if let Some(slot) = self.resolved.get_mut(index) {
            *slot = Some(label.to_owned());
        }
        if let Some(row) = self.rows.get_mut(index) {
            if !label.is_empty() {
                row.label = text::bounded(&text::clean(label), 120);
            }
            if category(effect).is_some() {
                row.category = category(effect);
            }
            row.state = state;
        }
        self.show(reply);
    }

    fn skip_from(&mut self, reply: &Reply, from: usize) {
        for row in self.rows.iter_mut().skip(from) {
            row.state = PageAssistStepStateV1::Skipped;
        }
        self.show(reply);
    }

    fn summary(&self) -> String {
        let rows: Vec<String> = self
            .rows
            .iter()
            .map(|row| {
                let state = match (row.state, row.reason) {
                    (PageAssistStepStateV1::Failed, Some(PageAssistStepReasonV1::Refused)) => {
                        "refused: a protected field or a link to another site"
                    }
                    (PageAssistStepStateV1::Failed, Some(PageAssistStepReasonV1::Unsuitable)) => {
                        "failed: that passage is not a control this verb can use"
                    }
                    (PageAssistStepStateV1::Failed, Some(PageAssistStepReasonV1::PageChanged)) => {
                        "failed: the page changed"
                    }
                    (PageAssistStepStateV1::Failed, _) => "ran, but the page did not change",
                    (PageAssistStepStateV1::Done, _) => "done",
                    (PageAssistStepStateV1::Declined, _) => "declined by the person",
                    (PageAssistStepStateV1::Skipped, _) => "skipped",
                    (PageAssistStepStateV1::Running | PageAssistStepStateV1::Confirm, _) => {
                        "stopped"
                    }
                };
                format!("{} — {state}", row.label)
            })
            .collect();
        rows.join("\n")
    }
}

/// A step's row before the engine judged it, named from the read.
fn planned_label(step: &Step, record: &ReadRecord) -> String {
    let name = step
        .target
        .and_then(|target| {
            record
                .readout
                .passages
                .iter()
                .find(|passage| passage.id == target.passage)
        })
        .map(|passage| text::bounded(&text::clean(&passage.text), 48))
        .filter(|name| !name.is_empty());
    let value = step
        .value
        .as_deref()
        .map(|value| text::bounded(&text::clean(value), 40));
    let quoted = |name: &str| format!("“{name}”");
    match (step.verb, name.as_deref(), value.as_deref()) {
        (Verb::Fill, Some(name), Some(value)) => {
            format!("Type {} into {}", quoted(value), quoted(name))
        }
        (Verb::Fill, None, Some(value)) => format!("Type {}", quoted(value)),
        (Verb::Select, Some(name), Some(value)) => {
            format!("Choose {} in {}", quoted(value), quoted(name))
        }
        (Verb::Submit, Some(name), _) => format!("Press Return in {}", quoted(name)),
        (Verb::Submit, None, _) => "Press Return".to_owned(),
        (Verb::Scroll, None, _) => match step.direction {
            Some(zephium_page_assist::actions::Direction::Up) => "Scroll up".to_owned(),
            _ => "Scroll down".to_owned(),
        },
        (verb, name, _) => {
            let word = match verb {
                Verb::Click => "Click",
                Verb::Fill => "Type",
                Verb::Select => "Choose",
                Verb::Check => "Check",
                Verb::Uncheck => "Uncheck",
                Verb::Scroll => "Scroll to",
                Verb::Expand => "Expand",
                Verb::Open => "Open",
                Verb::Submit => "Press Return",
            };
            match name {
                Some(name) => format!("{word} {}", quoted(name)),
                None => word.to_owned(),
            }
        }
    }
}

async fn decision(reply: &Reply, block: u16) -> Result<PageAssistDecisionV1, CancelReason> {
    reply.browser_admission().map_err(|_| CancelReason::Gone)?;
    let (sender, receiver) = oneshot::channel();
    reply
        .service
        .running_reply(&reply.conversation, reply.reply, |running| {
            running.decision = Some((block, sender))
        })
        .ok_or(CancelReason::Gone)?;
    reply.apply(PageAssistEventV1::State {
        state: PageAssistReplyStateV1::Waiting,
    });
    let human_wait = reply
        .native_budget
        .as_ref()
        .map(|budget| budget.wait(super::native_budget::HumanWaitKind::Confirmation))
        .transpose()
        .map_err(|_| CancelReason::Gone)?;
    let decided = tokio::select! {
        biased;
        _ = reply.browser_closed_wait() => Err(CancelReason::Gone),
        reason = reply.cancel.cancelled() => Err(reason),
        decision = receiver => decision.map_err(|_| CancelReason::Gone),
    };
    if let Some(wait) = human_wait {
        if decided.is_ok() {
            wait.finish().map_err(|_| CancelReason::Gone)?;
        }
    }
    if !reply.browser_closed() {
        reply.apply(PageAssistEventV1::State {
            state: PageAssistReplyStateV1::Working,
        });
    }
    decided
}

async fn act(reply: &Reply, text: &mut Text, steps: Vec<Step>) -> Result<ToolOutcome, End> {
    let first = steps
        .iter()
        .find_map(|step| step.target)
        .map(|target| target.read)
        .unwrap_or(0);
    if steps
        .iter()
        .any(|step| step.target.is_some_and(|target| target.read != first))
    {
        return Ok(Err(
            "All steps of one act call must be on one page.".to_owned()
        ));
    }
    let Some(record) = reply
        .with_data(|data| data.reads.get(usize::from(first)).cloned())
        .flatten()
    else {
        return Ok(Err("Read the page before acting on it.".to_owned()));
    };
    let requests: Vec<StepRequest> = steps
        .iter()
        .map(|step| StepRequest {
            verb: step.verb,
            read: record.readout.read,
            target: step.target.map(|target| target.passage),
            direction: step.direction,
            value: step.value.clone(),
        })
        .collect();
    let last = requests
        .iter()
        .position(|step| step.verb == Verb::Open)
        .map_or(requests.len(), |open| open + 1);
    let rows: Vec<PageAssistStepV1> = steps
        .iter()
        .take(last)
        .enumerate()
        .map(|(index, step)| PageAssistStepV1 {
            index: u8::try_from(index).unwrap_or(u8::MAX),
            label: planned_label(step, &record),
            category: None,
            state: PageAssistStepStateV1::Running,
            reason: None,
        })
        .collect();
    let block = reply.add_block(
        text,
        PageAssistBlockV1::Steps {
            steps: rows.clone(),
        },
    );
    let mut plan = Plan {
        tab: record.tab.clone(),
        epoch: record.epoch,
        record,
        requests,
        rows,
        resolved: Vec::new(),
        block,
    };
    plan.requests.truncate(last);
    plan.resolved = vec![None; plan.requests.len()];
    reply
        .service
        .running_reply(&reply.conversation, reply.reply, |running| {
            running.acting = Some(plan.tab.clone())
        });
    reply.leased.store(true, Ordering::Release);
    reply.phase(Some(PageAssistPhaseV1::Acting));
    reply.with_data(|data| data.working = Some(plan.tab.clone()));
    reply.glow(&plan.tab, Glow::Working);
    let result = run_steps(reply, text, &mut plan).await;
    reply.leased.store(false, Ordering::Release);
    reply
        .service
        .running_reply(&reply.conversation, reply.reply, |running| {
            running.acting = None
        });
    reply.phase(Some(PageAssistPhaseV1::Thinking));
    match result {
        Ok(()) => Ok(Ok(plan.summary())),
        Err(Some(end)) => Err(end),
        Err(None) => Ok(Err(plan.summary())),
    }
}

/// `Err(None)` ends the steps and tells the model; `Err(Some)` ends the reply.
async fn run_steps(reply: &Reply, text: &mut Text, plan: &mut Plan) -> Result<(), Option<End>> {
    let page = reply.service.ports().page.clone();
    for index in 0..plan.requests.len() {
        reply
            .browser_admission()
            .map_err(|fault| Some(native_fault(fault)))?;
        if reply.took_over.swap(false, Ordering::AcqRel) {
            take_over(reply, text, plan, index).await?;
        }
        let step = plan.requests[index].clone();
        let resolved = admissible(&plan.record, &step)
            .and_then(|()| page.resolve(&plan.tab, plan.epoch, &step));
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(failure) => {
                plan.fail(reply, index, failure);
                return Err(None);
            }
        };
        if let StepEffect::Commit(_) = resolved.effect {
            plan.mark(
                reply,
                index,
                &resolved.label,
                resolved.effect,
                PageAssistStepStateV1::Confirm,
            );
            match decision(reply, plan.block).await {
                Ok(PageAssistDecisionV1::Approve) => {}
                Ok(PageAssistDecisionV1::Decline) => {
                    plan.mark(
                        reply,
                        index,
                        &resolved.label,
                        resolved.effect,
                        PageAssistStepStateV1::Declined,
                    );
                    plan.skip_from(reply, index + 1);
                    return Err(None);
                }
                Err(reason) => return Err(Some(End::Cancelled(reason))),
            }
        }
        plan.mark(
            reply,
            index,
            &resolved.label,
            resolved.effect,
            PageAssistStepStateV1::Running,
        );
        reply
            .browser_admission()
            .map_err(|fault| Some(native_fault(fault)))?;
        match page.act(&plan.tab, plan.epoch, &step).await {
            Ok(()) => plan.mark(
                reply,
                index,
                &resolved.label,
                resolved.effect,
                PageAssistStepStateV1::Done,
            ),
            Err(failure) => {
                plan.fail(reply, index, failure);
                return Err(None);
            }
        }
    }
    Ok(())
}

/// The person used the page: wait for Continue, then read it again and go
/// on only if every remaining step still names the same control.
async fn take_over(
    reply: &Reply,
    text: &mut Text,
    plan: &mut Plan,
    from: usize,
) -> Result<(), Option<End>> {
    reply
        .browser_admission()
        .map_err(|fault| Some(native_fault(fault)))?;
    reply.add_block(
        text,
        PageAssistBlockV1::Paused {
            reason: PageAssistPauseV1::TookOver,
        },
    );
    reply.apply(PageAssistEventV1::State {
        state: PageAssistReplyStateV1::Paused,
    });
    reply.glow(&plan.tab, Glow::Paused);
    let (sender, receiver) = oneshot::channel();
    reply
        .service
        .running_reply(&reply.conversation, reply.reply, |running| {
            running.resume = Some(sender)
        })
        .ok_or(Some(End::Cancelled(CancelReason::Gone)))?;
    let human_wait = reply
        .native_budget
        .as_ref()
        .map(|budget| budget.wait(super::native_budget::HumanWaitKind::Takeover))
        .transpose()
        .map_err(|_| Some(native_fault(PageAssistNativeFaultV1::Timeout)))?;
    tokio::select! {
        biased;
        _ = reply.browser_closed_wait() => return Err(Some(End::Cancelled(CancelReason::Gone))),
        reason = reply.cancel.cancelled() => return Err(Some(End::Cancelled(reason))),
        resumed = receiver => resumed.map_err(|_| Some(End::Cancelled(CancelReason::Gone)))?,
    }
    if let Some(wait) = human_wait {
        wait.finish()
            .map_err(|_| Some(native_fault(PageAssistNativeFaultV1::Timeout)))?;
    }
    reply
        .browser_admission()
        .map_err(|fault| Some(native_fault(fault)))?;
    reply.glow(&plan.tab, Glow::Working);
    reply.apply(PageAssistEventV1::State {
        state: PageAssistReplyStateV1::Working,
    });
    let labels: Vec<Option<String>> = plan.resolved.iter().skip(from).cloned().collect();
    let index = reply.read_tab(&plan.tab).await.map_err(|_| None)?;
    let Some(fresh) = reply
        .with_data(|data| data.reads.get(index).cloned())
        .flatten()
    else {
        return Err(None);
    };
    reply.phase(Some(PageAssistPhaseV1::Acting));
    for step in plan.requests.iter_mut().skip(from) {
        step.read = fresh.readout.read;
    }
    plan.epoch = fresh.epoch;
    plan.record = fresh;
    let page = reply.service.ports().page.clone();
    for (step, label) in plan.requests.iter().skip(from).zip(labels) {
        match (page.resolve(&plan.tab, plan.epoch, step), label) {
            (Ok(resolved), Some(label)) if resolved.label == label => {}
            (Ok(_), None) => {}
            _ => return Err(None),
        }
    }
    Ok(())
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    #[test]
    fn absent_measurement_is_sticky_and_distinct_from_reported_zero() {
        let mut usage = WorkModelUsage::default();
        assert!(usage.is_reported());
        add_usage(
            &mut usage,
            &WorkModelUsage {
                input_tokens: 17,
                cost_micros: Some(20),
                ..WorkModelUsage::default()
            },
        );
        assert_eq!(usage.input_tokens, 17);
        add_usage(&mut usage, &WorkModelUsage::unavailable());
        assert!(!usage.is_reported());
        assert_eq!(usage.cost_micros, None);
        add_usage(
            &mut usage,
            &WorkModelUsage {
                input_tokens: 99,
                cost_micros: Some(80),
                ..WorkModelUsage::default()
            },
        );
        assert!(!usage.is_reported());
        assert_eq!(usage.cost_micros, None);
        assert_eq!(
            usage.input_tokens, 17,
            "unavailable aggregate counters are not interpreted"
        );
        let mut missing = WorkModelUsage::unavailable();
        add_usage(&mut missing, &WorkModelUsage::default());
        assert!(!missing.is_reported());
    }
}
