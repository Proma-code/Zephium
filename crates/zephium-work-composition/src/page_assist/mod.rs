//! The assistant's service: one conversation per Space, a tool-using model loop per
//! message, and every effect on a page (marks, steps, styles, new tabs)
//! admitted here against what was actually read. Whatever commits waits for
//! the person; everything stops on Stop, AI off or a closed conversation.
#![deny(clippy::print_stderr, clippy::print_stdout, clippy::dbg_macro)]

mod agent;
mod fault;
mod native_browser;
mod native_budget;
pub mod port;
mod preparation;
pub use zephium_page_assist as core;
mod readout;
pub mod steps;
mod tools;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{oneshot, Notify};
use zephium_core::ids::ProfileId;
use zephium_core::work::model::WorkModelReasoning;
use zephium_ipc::page_assist::*;
use zephium_page_assist::citations::CitationBook;
use zephium_page_assist::conversation::Conversation;
use zephium_page_assist::outline::Outline;
use zephium_page_assist::page::{PageRead, PageTraits};
use zephium_page_assist::policy::{self, TabFacts as PolicyFacts};
use zephium_page_assist::{suggest, text};

use port::*;

const VERSION: u16 = 2;
/// Most styles one site keeps.
pub const MAX_SITE_STYLES: usize = 8;
/// Most validated CSS one site keeps across its styles, in bytes, so its
/// stored row stays inside a settings value.
pub const MAX_SITE_CSS_BYTES: usize = 48 * 1024;
/// Conversations kept in memory at once.
const MAX_LIVE: usize = 12;
/// Native reservation admission is bounded separately from the running reply.
const NATIVE_RESERVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
/// Addresses a conversation may open, from its reads and the person's words.
const MAX_LINKS: usize = 4096;
/// Conversations the history list shows.
const HISTORY: u32 = 100;
/// Picked elements one tab keeps attached.
const MAX_PICKS: usize = 3;

/// The ports the service works through.
#[derive(Clone)]
pub struct Ports {
    /// Live tabs.
    pub page: Arc<dyn PagePort>,
    /// The Space's tab list and new tabs.
    pub tabs: Arc<dyn Tabs>,
    /// The person's Page model.
    pub models: Arc<dyn ModelSource>,
    /// The panel's event channel.
    pub events: Arc<dyn EventSink>,
    /// Time.
    pub clock: Arc<dyn Clock>,
    /// The shell's switches.
    pub settings: Arc<dyn Settings>,
    /// Starts Work.
    pub work: Arc<dyn WorkHandoff>,
    /// Page-style persistence.
    pub styles: Arc<dyn StyleStore>,
    /// Conversation persistence.
    pub conversations: Arc<dyn ConversationStore>,
    /// The app's log.
    pub diagnostics: Arc<dyn Diagnostics>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CancelReason {
    Stopped,
    Gone,
}

#[derive(Default)]
struct Cancel {
    // Same cancellation-only bit observed by the original writer admission.
    native: Arc<AtomicBool>,
    reason: Mutex<Option<CancelReason>>,
    notify: Notify,
}

impl Cancel {
    fn cancel(&self, reason: CancelReason) {
        self.native.store(true, Ordering::Release);
        let mut slot = self.reason.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(reason);
        }
        drop(slot);
        self.notify.notify_waiters();
    }

    fn reason(&self) -> Option<CancelReason> {
        *self.reason.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn cancelled(&self) -> CancelReason {
        loop {
            let notified = self.notify.notified();
            if let Some(reason) = self.reason() {
                return reason;
            }
            notified.await;
        }
    }
}

type DecisionSender = oneshot::Sender<PageAssistDecisionV1>;

struct Running {
    reply: u32,
    profile: ProfileId,
    native_browser: bool,
    cancel: Arc<Cancel>,
    /// A step's confirm, by its block.
    decision: Option<(u16, DecisionSender)>,
    resume: Option<oneshot::Sender<()>>,
    /// Steps are running: human input in their tab pauses them.
    leased: Arc<AtomicBool>,
    took_over: Arc<AtomicBool>,
    acting: Option<String>,
}

/// One page read in a reply, under its letters.
#[derive(Clone)]
struct ReadRecord {
    tab: String,
    epoch: u64,
    readout: Readout,
    page: PageRead,
}

struct PendingStyle {
    profile: ProfileId,
    site: String,
    summary: String,
    css: String,
    rules: u16,
}

#[derive(Default)]
struct ReplyData {
    book: CitationBook,
    reads: Vec<ReadRecord>,
    /// `t1`, `t2`… in the order the reply met the tabs.
    handles: Vec<String>,
    outlines: HashMap<usize, Outline>,
    styles: HashMap<u16, PendingStyle>,
    handoffs: HashMap<u16, WorkStart>,
    credits: Option<String>,
    /// Source-specific terminal fact; never an effect-admission capability.
    native_outcome: Option<NativeConversationOutcome>,
    /// The tab the reply works in: the attached page, then wherever go_to went.
    working: Option<String>,
}

struct Live {
    profile: ProfileId,
    conversation: Conversation,
    /// Nonwrapping observation identity; renewed for every transcript mutation.
    revision: Arc<()>,
    replies: HashMap<u32, ReplyData>,
    links: HashSet<String>,
    touched: u64,
}

struct PickRecord {
    epoch: u64,
    label: String,
}

#[derive(Default)]
struct State {
    live: HashMap<String, Live>,
    /// Each Space's conversation in the panel.
    current: HashMap<String, String>,
    /// Changes when a Space leaves/reopens a conversation, including A→B→A.
    current_epochs: HashMap<String, Arc<()>>,
    running: HashMap<String, Running>,
    /// Uncommitted explicit asks; lifecycle only, never native admission.
    preparing: HashMap<String, preparation::Preparing>,
    /// Entered native browser owners outlive cancelled/dropped UI waiters.
    native_owners: HashMap<(String, u32), agent::BrowserOwner>,
    /// The structure of each tab's latest read, for suggestions.
    traits: HashMap<String, (u64, PageTraits)>,
    /// Which items block owns each tab's marks.
    marks: HashMap<String, (String, u32, u16)>,
    picks: HashMap<(String, u64), PickRecord>,
    styles: HashMap<ProfileId, Vec<StoredStyle>>,
    next_style: u64,
    next_conversation: u64,
}

struct Shared {
    state: Mutex<State>,
    ports: Ports,
}

/// The assistant's service.
#[derive(Clone)]
pub struct PageAssist {
    shared: Arc<Shared>,
}

/// A reply's work, for the caller to spawn on its runtime.
pub type TurnTask = port::BoxFuture<'static, ()>;

fn site_of(url: Option<&str>) -> Option<String> {
    url.and_then(zephium_app::work_sites::site_of)
}

fn host_of(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(host, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) {
            host
        } else {
            rest
        }
    });
    host.strip_prefix("www.")
        .unwrap_or(host)
        .to_ascii_lowercase()
}

fn effort_name(effort: WorkModelReasoning) -> &'static str {
    match effort {
        WorkModelReasoning::Low => "low",
        WorkModelReasoning::Medium => "medium",
        WorkModelReasoning::High => "high",
        WorkModelReasoning::XHigh => "xhigh",
        WorkModelReasoning::Max => "max",
    }
}

/// Web addresses the person wrote.
fn written_links(words: &str) -> impl Iterator<Item = String> + '_ {
    words
        .split_whitespace()
        .map(|word| {
            word.trim_start_matches(['(', '<', '"', '\''])
                .trim_end_matches(['.', ',', ';', ':', ')', '>', '"', '\'', '!', '?'])
        })
        .filter(|word| word.starts_with("http://") || word.starts_with("https://"))
        .filter(|word| policy::is_web_url(word) && word.len() <= 2048)
        .map(str::to_owned)
}

enum Admitted {
    Page {
        tab: String,
    },
    Selection {
        tab: String,
        text: String,
    },
    Element {
        tab: String,
        host: String,
        label: String,
        pick: Pick,
    },
}

impl PageAssist {
    /// A service over `ports`.
    pub fn new(ports: Ports) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                ports,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn ports(&self) -> &Ports {
        &self.shared.ports
    }

    fn emit_all(&self, events: impl IntoIterator<Item = PageAssistChangedV1>) {
        for event in events {
            self.ports().events.emit(event);
        }
    }

    /// Folds an event into `reply` of `conversation` and sends it.
    fn apply(&self, conversation: &str, reply: u32, kind: PageAssistEventV1) -> bool {
        let event = {
            let mut state = self.state();
            state.live.get_mut(conversation).and_then(|live| {
                let event = live.conversation.apply(reply, kind);
                if event.is_some() {
                    live.revision = Arc::new(());
                }
                event
            })
        };
        let sent = event.is_some();
        self.emit_all(event);
        sent
    }

    fn conversation_wide(
        &self,
        conversation: &Conversation,
        kind: PageAssistEventV1,
    ) -> PageAssistChangedV1 {
        PageAssistChangedV1 {
            space: conversation.space().to_owned(),
            conversation: conversation.id().to_owned(),
            reply: 0,
            seq: 0,
            kind,
        }
    }

    /// Page-local safety alone. It does not observe or wake a provider.
    fn local_availability(&self, facts: Option<&TabFacts>) -> PageAssistAvailabilityV1 {
        let Some(facts) = facts else {
            return PageAssistAvailabilityV1::Unavailable;
        };
        let site = site_of(facts.url.as_deref());
        policy::availability(&PolicyFacts {
            ai_enabled: self.ports().settings.ai_enabled(&facts.profile),
            private: facts.private,
            url: facts.url.as_deref(),
            viewer: facts.viewer || site.is_none(),
            sensitive: site
                .as_deref()
                .is_some_and(zephium_app::work_sites::is_sensitive),
            loading: facts.loading,
            has_model: true,
            supported: self.ports().page.supported(),
        })
    }

    /// Active reply/tool admission never accepts retained dormant metadata.
    async fn availability(&self, facts: Option<&TabFacts>) -> PageAssistAvailabilityV1 {
        let local = self.local_availability(facts);
        if local != PageAssistAvailabilityV1::Ready {
            return local;
        }
        let Some(facts) = facts else {
            return PageAssistAvailabilityV1::Unavailable;
        };
        if self.ports().models.entry(&facts.profile).await.is_some() {
            PageAssistAvailabilityV1::Ready
        } else {
            PageAssistAvailabilityV1::NoModel
        }
    }

    /// A parked runtime does not revoke the browser's own local draft controls.
    /// Neither observation below may wake a runtime or reserve/send a model.
    async fn draft_availability(&self, facts: Option<&TabFacts>) -> PageAssistAvailabilityV1 {
        let active = self.availability(facts).await;
        if active != PageAssistAvailabilityV1::NoModel {
            return active;
        }
        let Some(facts) = facts else {
            return PageAssistAvailabilityV1::Unavailable;
        };
        if self.ports().models.dormant(&facts.profile).await.is_some() {
            PageAssistAvailabilityV1::Dormant
        } else {
            active
        }
    }

    fn draft_scope_current(&self, space: &str, tab: &str, captured: &TabFacts) -> bool {
        self.ports().tabs.profile(space) == Some(captured.profile)
            && captured.space.as_deref().is_none_or(|s| s == space)
            && self.ports().page.facts(tab).is_some_and(|facts| {
                facts.profile == captured.profile
                    && facts.epoch == captured.epoch
                    && facts.space == captured.space
                    && facts.url == captured.url
                    && self.local_availability(Some(&facts)) == PageAssistAvailabilityV1::Ready
            })
    }

    fn new_id(&self) -> String {
        let mut state = self.state();
        state.next_conversation += 1;
        format!(
            "c{:x}-{:x}",
            self.ports().clock.unix_ms(),
            state.next_conversation
        )
    }

    /// Keeps at most [`MAX_LIVE`] conversations in memory: the least recently
    /// used that is neither shown nor running leaves first.
    fn evict(state: &mut State) {
        while state.live.len() > MAX_LIVE {
            let shown: HashSet<&String> = state.current.values().collect();
            let Some(oldest) = state
                .live
                .iter()
                .filter(|(id, _)| {
                    !shown.contains(id)
                        && !state.running.contains_key(*id)
                        && !state.preparing.contains_key(*id)
                })
                .min_by_key(|(_, live)| live.touched)
                .map(|(id, _)| id.clone())
            else {
                return;
            };
            state.live.remove(&oldest);
        }
    }

    /// The Space's conversation, made empty when it has none.
    fn current(&self, space: &str, profile: ProfileId) -> String {
        let now = self.ports().clock.now_ms();
        let existing = {
            let mut state = self.state();
            let id = state.current.get(space).cloned();
            match id {
                Some(id)
                    if state
                        .live
                        .get(&id)
                        .is_some_and(|live| live.profile == profile) =>
                {
                    if let Some(live) = state.live.get_mut(&id) {
                        live.touched = now;
                    }
                    Some(id)
                }
                _ => {
                    if let Some(id) = state.current.get(space) {
                        if let Some(preparing) = state.preparing.get(id) {
                            preparing.cancel.cancel(CancelReason::Gone);
                        }
                    }
                    None
                }
            }
        };
        if let Some(id) = existing {
            return id;
        }
        let id = self.new_id();
        let mut state = self.state();
        state.live.insert(
            id.clone(),
            Live {
                profile,
                conversation: Conversation::new(id.clone(), space.to_owned()),
                revision: Arc::new(()),
                replies: HashMap::new(),
                links: HashSet::new(),
                touched: now,
            },
        );
        state.current.insert(space.to_owned(), id.clone());
        state.current_epochs.insert(space.to_owned(), Arc::new(()));
        Self::evict(&mut state);
        id
    }

    fn profile_for(&self, space: &str, facts: Option<&TabFacts>) -> Option<ProfileId> {
        facts
            .map(|facts| facts.profile)
            .or_else(|| self.ports().tabs.profile(space))
    }

    /// Everything the panel shows for `space` with `tab` in front.
    pub async fn status(&self, space: &str, tab: Option<&str>) -> PageAssistStatusV1 {
        let facts = tab.and_then(|tab| self.ports().page.facts(tab));
        let profile = self.profile_for(space, facts.as_ref());
        let availability = if profile
            .is_some_and(|profile| self.ports().tabs.profile(space) != Some(profile))
            || facts
                .as_ref()
                .is_some_and(|facts| facts.space.as_deref().is_some_and(|s| s != space))
        {
            PageAssistAvailabilityV1::Unavailable
        } else {
            self.draft_availability(facts.as_ref()).await
        };
        let mut status = PageAssistStatusV1 {
            version: VERSION,
            space: space.to_owned(),
            tab: tab.map(str::to_owned),
            availability,
            page: None,
            model: None,
            suggestions: Vec::new(),
            selection: None,
            can_pick: false,
            styled: false,
            conversation: PageAssistConversationV1 {
                id: String::new(),
                title: None,
                messages: Vec::new(),
                seq: 0,
            },
        };
        let Some(profile) = profile else {
            return status;
        };
        if self.ports().tabs.profile(space) != Some(profile)
            || facts.as_ref().is_some_and(|facts| {
                facts.private || facts.space.as_deref().is_some_and(|s| s != space)
            })
            || !self.ports().settings.ai_enabled(&profile)
        {
            return status;
        }
        let id = self.current(space, profile);
        if let Some(view) = self
            .state()
            .live
            .get(&id)
            .map(|live| live.conversation.view().clone())
        {
            status.conversation = view;
        }
        status.model = self
            .ports()
            .models
            .entry(&profile)
            .await
            .map(|choice| match choice {
                PageBackendChoice::Model(choice) => PageAssistModelV1 {
                    id: choice.entry.id.clone(),
                    hosted: choice.entry.model.provider
                        == zephium_core::work::model::WorkModelProvider::Cloud,
                    name: choice.entry.display_name,
                    source: PageAssistModelSourceV1::Model,
                    capabilities: Some(PageAssistCapabilitiesV1::Page),
                    effort: choice
                        .effort
                        .filter(|_| choice.entry.supports.reasoning)
                        .map(|effort| effort_name(effort).to_owned()),
                },
                PageBackendChoice::NativeConversation(choice) => PageAssistModelV1 {
                    id: choice.id,
                    name: choice.name,
                    hosted: false,
                    source: choice.source,
                    capabilities: Some(choice.capabilities),
                    effort: choice.effort,
                },
            });
        if status.model.is_none() {
            status.model =
                self.ports()
                    .models
                    .dormant(&profile)
                    .await
                    .map(|choice| PageAssistModelV1 {
                        id: choice.id,
                        name: choice.name,
                        hosted: false,
                        source: choice.source,
                        capabilities: None,
                        effort: choice.effort,
                    });
        }
        if matches!(
            availability,
            PageAssistAvailabilityV1::Ready
                | PageAssistAvailabilityV1::Dormant
                | PageAssistAvailabilityV1::NoModel
        ) {
            status.availability = match &status.model {
                Some(model) if model.capabilities.is_none() => PageAssistAvailabilityV1::Dormant,
                Some(_) => PageAssistAvailabilityV1::Ready,
                None => PageAssistAvailabilityV1::NoModel,
            };
        }
        if self.ports().tabs.profile(space) != Some(profile)
            || !self.ports().settings.ai_enabled(&profile)
        {
            status.model = None;
            status.availability = PageAssistAvailabilityV1::Unavailable;
            return status;
        }
        if status
            .model
            .as_ref()
            .is_some_and(|model| model.capabilities == Some(PageAssistCapabilitiesV1::TextOnly))
        {
            // Plain original-client chat needs no page availability or reads.
            status.availability = PageAssistAvailabilityV1::Ready;
            return status;
        }
        let (Some(tab), Some(facts)) = (tab, facts) else {
            return status;
        };
        if !matches!(
            status.availability,
            PageAssistAvailabilityV1::Ready | PageAssistAvailabilityV1::Dormant
        ) || !self.draft_scope_current(space, tab, &facts)
        {
            return status;
        }
        let url = facts.url.clone().unwrap_or_default();
        let site = site_of(Some(&url));
        status.page = Some(PageAssistPageV1 {
            tab: tab.to_owned(),
            host: host_of(&url),
            title: text::bounded(
                &text::clean(facts.title.as_deref().unwrap_or_default()),
                120,
            ),
        });
        status.can_pick = true;
        if let Some(site) = &site {
            status.styled = self
                .styles_of(&facts.profile)
                .iter()
                .any(|style| style.enabled && &style.site == site);
        }
        let cached = self
            .state()
            .traits
            .get(tab)
            .filter(|(epoch, _)| *epoch == facts.epoch)
            .map(|(_, traits)| *traits);
        let traits = match cached {
            Some(traits) => Some(traits),
            None => match self.ports().page.read(tab, facts.epoch).await {
                Ok(readout)
                    if readout.epoch == facts.epoch
                        && self.draft_scope_current(space, tab, &facts) =>
                {
                    let read = readout::page_read(
                        "a",
                        site.as_deref().unwrap_or_default(),
                        &host_of(&url),
                        &readout,
                        &zephium_agentic::contains_recognized_secret,
                    );
                    self.state()
                        .traits
                        .insert(tab.to_owned(), (facts.epoch, read.traits));
                    Some(read.traits)
                }
                _ => None,
            },
        };
        if !self.draft_scope_current(space, tab, &facts) {
            status.can_pick = false;
            status.page = None;
            status.availability = PageAssistAvailabilityV1::Unavailable;
            return status;
        }
        let room = site
            .as_deref()
            .is_some_and(|site| self.site_has_room(&facts.profile, site, 0));
        status.suggestions = suggest::suggestions(&traits.unwrap_or_default(), room);
        status.selection = self
            .ports()
            .page
            .selection(tab, facts.epoch)
            .await
            .map(|selection| text::bounded(&text::clean(&selection), PAGE_ASSIST_MAX_EXCERPT_CHARS))
            .filter(|selection| !selection.is_empty());
        if !self.draft_scope_current(space, tab, &facts) {
            status.selection = None;
            status.suggestions.clear();
            status.can_pick = false;
            status.page = None;
            status.availability = PageAssistAvailabilityV1::Unavailable;
        }
        status
    }

    async fn admit(
        &self,
        profile: ProfileId,
        attach: &PageAssistAttachV1,
    ) -> Result<(Admitted, PageAssistAttachmentV1), PageAssistFaultV1> {
        let tab = match attach {
            PageAssistAttachV1::Page { tab }
            | PageAssistAttachV1::Element { tab, .. }
            | PageAssistAttachV1::Selection { tab } => tab.clone(),
        };
        let facts = self
            .ports()
            .page
            .facts(&tab)
            .filter(|facts| facts.profile == profile)
            .ok_or(PageAssistFaultV1::Invalid)?;
        if let Some(fault) = policy::refusal(self.availability(Some(&facts)).await) {
            return Err(fault);
        }
        let url = facts.url.clone().unwrap_or_default();
        let host = host_of(&url);
        let title = text::bounded(
            &text::clean(facts.title.as_deref().unwrap_or_default()),
            120,
        );
        Ok(match attach {
            PageAssistAttachV1::Page { .. } => (
                Admitted::Page { tab: tab.clone() },
                PageAssistAttachmentV1::Page { tab, host, title },
            ),
            PageAssistAttachV1::Selection { .. } => {
                let text = self
                    .ports()
                    .page
                    .selection(&tab, facts.epoch)
                    .await
                    .filter(|selection| !selection.trim().is_empty())
                    .ok_or(PageAssistFaultV1::NoSelection)?;
                let excerpt = text::bounded(&text::clean(&text), PAGE_ASSIST_MAX_EXCERPT_CHARS);
                (
                    Admitted::Selection {
                        tab: tab.clone(),
                        text,
                    },
                    PageAssistAttachmentV1::Selection { tab, excerpt, host },
                )
            }
            PageAssistAttachV1::Element { pick, .. } => {
                let id: u64 = pick.parse().map_err(|_| PageAssistFaultV1::Invalid)?;
                let label = {
                    let state = self.state();
                    let record = state
                        .picks
                        .get(&(tab.clone(), id))
                        .filter(|record| record.epoch == facts.epoch)
                        .ok_or(PageAssistFaultV1::PageChanged)?;
                    record.label.clone()
                };
                let picked = self
                    .ports()
                    .page
                    .pick_read(&tab, facts.epoch, id)
                    .await
                    .ok_or(PageAssistFaultV1::PageChanged)?;
                (
                    Admitted::Element {
                        tab: tab.clone(),
                        host: host.clone(),
                        label: label.clone(),
                        pick: picked,
                    },
                    PageAssistAttachmentV1::Element {
                        tab,
                        pick: pick.clone(),
                        label,
                        host,
                    },
                )
            }
        })
    }

    /// Admits a message. On success the person's message and the reply's
    /// start have been sent, and the returned task, once spawned, produces
    /// the rest.
    pub async fn ask(
        &self,
        request: PageAssistAskV1,
    ) -> Result<(u32, TurnTask), PageAssistFaultV1> {
        self.ask_native_mode(request, true).await
    }

    /// Native Page uses only BrowserV1. The private false branch is retained for
    /// historical text-contract fixtures; no public route falls back to it.
    async fn ask_native_mode(
        &self,
        request: PageAssistAskV1,
        browser: bool,
    ) -> Result<(u32, TurnTask), PageAssistFaultV1> {
        if request.text.len() > PAGE_ASSIST_MAX_QUESTION_BYTES
            || request.attachments.len() > PAGE_ASSIST_MAX_ATTACHMENTS
            || (request.text.trim().is_empty() && request.suggestion.is_none())
        {
            return Err(PageAssistFaultV1::Invalid);
        }
        let space = request.space.clone();
        let (profile, current_epoch, revision, previous_reply, new_conversation) = {
            let state = self.state();
            let live = state
                .current
                .get(&space)
                .filter(|id| **id == request.conversation)
                .and_then(|id| state.live.get(id))
                .ok_or(PageAssistFaultV1::Invalid)?;
            (
                live.profile,
                state.current_epochs.get(&space).cloned(),
                live.revision.clone(),
                live.conversation.latest_reply(),
                live.conversation.messages().is_empty(),
            )
        };
        let mut preparation = preparation::Preparation::begin(self, &space, &request.conversation)?;
        if !Arc::ptr_eq(&preparation.revision, &revision)
            || current_epoch
                .as_ref()
                .is_none_or(|epoch| !Arc::ptr_eq(epoch, &preparation.current_epoch))
        {
            return Err(PageAssistFaultV1::NativeConversation {
                fault: PageAssistNativeFaultV1::ScopeChanged,
            });
        }
        if self
            .state()
            .native_owners
            .values()
            .any(|owner| owner.blocks(profile))
        {
            return Err(PageAssistFaultV1::Busy);
        }
        if !self.ports().settings.ai_enabled(&profile) {
            return Err(PageAssistFaultV1::AiOff);
        }
        let demand = if browser {
            preparation
                .wait_until(
                    preparation.deadline,
                    self.ports().models.prepare_reply(
                        &profile,
                        preparation.cancel.native.clone(),
                        preparation.deadline,
                    ),
                )
                .await?
                .map_err(preparation::backend_fault)?
        } else {
            None
        };
        if demand.is_some() {
            preparation.native_demand();
        }
        let backend = preparation
            .wait(self.ports().models.entry(&profile))
            .await?
            .ok_or_else(|| {
                if demand.is_some() {
                    PageAssistFaultV1::NativeConversation {
                        fault: PageAssistNativeFaultV1::ScopeChanged,
                    }
                } else {
                    PageAssistFaultV1::NoModel
                }
            })?;
        if browser
            && demand.is_some()
            && !matches!(&backend, PageBackendChoice::NativeConversation(_))
        {
            // A demand cannot migrate across a source change or be replaced by
            // a new native reservation after the owner was allowed to park.
            return Err(PageAssistFaultV1::NativeConversation {
                fault: PageAssistNativeFaultV1::ScopeChanged,
            });
        }
        let words = match &backend {
            PageBackendChoice::NativeConversation(_) => {
                if !browser && (request.suggestion.is_some() || !request.attachments.is_empty()) {
                    return Err(PageAssistFaultV1::NativeConversation {
                        fault: PageAssistNativeFaultV1::UnsupportedInput,
                    });
                }
                request.text.clone()
            }
            PageBackendChoice::Model(_) => text::clean(&request.text),
        };
        if words.trim().is_empty() && request.suggestion.is_none() {
            return Err(PageAssistFaultV1::Invalid);
        }
        let reservation = if let PageBackendChoice::NativeConversation(expected) = &backend {
            let required = if browser {
                PageAssistCapabilitiesV1::Page
            } else {
                PageAssistCapabilitiesV1::TextOnly
            };
            if expected.capabilities != required {
                // No text fallback or renderer-selected escalation of a prior
                // text-only durable conversation. Native reservation still joins
                // the exact current policy/manifest/history after this projection.
                return Err(PageAssistFaultV1::NativeConversation {
                    fault: PageAssistNativeFaultV1::UnsupportedInput,
                });
            }
            {
                let state = self.state();
                if state.current.get(&space) != Some(&request.conversation)
                    || current_epoch
                        .as_ref()
                        .zip(state.current_epochs.get(&space))
                        .is_none_or(|(a, b)| !Arc::ptr_eq(a, b))
                    || state.live.get(&request.conversation).is_none_or(|live| {
                        live.profile != profile || !Arc::ptr_eq(&live.revision, &revision)
                    })
                {
                    return Err(PageAssistFaultV1::NativeConversation {
                        fault: PageAssistNativeFaultV1::ScopeChanged,
                    });
                }
                if state.running.contains_key(&request.conversation) {
                    return Err(PageAssistFaultV1::Busy);
                }
            }
            let draft = NativeConversationDraft {
                conversation: request.conversation.clone(),
                new_conversation,
                previous_reply,
                text: words.clone(),
            };
            let reserve = if browser {
                demand
                    .ok_or(PageAssistFaultV1::NativeConversation {
                        fault: PageAssistNativeFaultV1::ScopeChanged,
                    })?
                    .reserve_browser(expected.clone(), draft, NativeBrowserManifest::page_v1())
            } else {
                self.ports()
                    .models
                    .reserve_native(&profile, expected.clone(), draft)
            };
            Some(
                preparation
                    .wait_until(std::time::Instant::now() + NATIVE_RESERVE_TIMEOUT, reserve)
                    .await?
                    .map_err(preparation::backend_fault)?,
            )
        } else {
            None
        };
        let mut admitted = Vec::new();
        let mut shown = Vec::new();
        for attach in &request.attachments {
            let (inner, view) = preparation.wait(self.admit(profile, attach)).await??;
            admitted.push(inner);
            shown.push(view);
        }
        let conversation = request.conversation.clone();
        let cancel = preparation.cancel.clone();
        let leased = Arc::new(AtomicBool::new(false));
        let took_over = Arc::new(AtomicBool::new(false));
        let title = request
            .suggestion
            .filter(|_| words.is_empty())
            .map(suggest::request_text);
        let (reply, events) = {
            let mut state = self.state();
            if let Some(reservation) = &reservation {
                if !self.ports().settings.ai_enabled(&profile) {
                    return Err(PageAssistFaultV1::AiOff);
                }
                if state.current.get(&space) != Some(&conversation)
                    || current_epoch
                        .as_ref()
                        .zip(state.current_epochs.get(&space))
                        .is_none_or(|(a, b)| !Arc::ptr_eq(a, b))
                    || state.live.get(&conversation).is_none_or(|live| {
                        live.profile != profile
                            || !Arc::ptr_eq(&live.revision, &revision)
                            || live.conversation.latest_reply() != previous_reply
                            || live.conversation.messages().is_empty() != new_conversation
                    })
                {
                    return Err(PageAssistFaultV1::NativeConversation {
                        fault: PageAssistNativeFaultV1::ScopeChanged,
                    });
                }
                reservation
                    .validate()
                    .map_err(|fault| PageAssistFaultV1::NativeConversation { fault })?;
            }
            if state.running.contains_key(&conversation)
                || state
                    .native_owners
                    .values()
                    .any(|owner| owner.blocks(profile))
                || state
                    .running
                    .values()
                    .any(|running| running.native_browser && running.profile == profile)
            {
                return Err(PageAssistFaultV1::Busy);
            }
            preparation.commit(&mut state)?;
            let Some(live) = state.live.get_mut(&conversation) else {
                return Err(PageAssistFaultV1::Invalid);
            };
            for link in written_links(&request.text) {
                if live.links.len() < MAX_LINKS {
                    live.links.insert(link);
                }
            }
            let mut events = live
                .conversation
                .say(&words, request.suggestion, shown, title);
            let (reply, started) = live.conversation.begin_reply();
            live.revision = Arc::new(());
            events.push(started);
            live.replies.insert(reply, ReplyData::default());
            state.running.insert(
                conversation.clone(),
                Running {
                    reply,
                    profile,
                    native_browser: browser && reservation.is_some(),
                    cancel: cancel.clone(),
                    decision: None,
                    resume: None,
                    leased: leased.clone(),
                    took_over: took_over.clone(),
                    acting: None,
                },
            );
            (reply, events)
        };
        self.emit_all(events);
        for attachment in &admitted {
            if let Admitted::Element { tab, pick, .. } = attachment {
                self.ports().page.pick_clear(tab, pick.pick);
                self.state().picks.remove(&(tab.clone(), pick.pick));
            }
        }
        let context = agent::Reply {
            service: self.clone(),
            conversation,
            space,
            profile,
            current_epoch,
            reply,
            words,
            native: Mutex::new(None),
            native_budget: (browser && reservation.is_some())
                .then(native_budget::NativeBudget::new),
            native_admission: (browser && reservation.is_some())
                .then(|| Arc::new(native_browser::NativeBrowserAdmission::default())),
            backend,
            suggestion: request.suggestion,
            attachments: admitted,
            cancel,
            leased,
            took_over,
            glowing: Mutex::new(Vec::new()),
        };
        if let Some(reservation) = reservation {
            if browser {
                agent::commit_native_browser(&context, reservation);
            } else {
                agent::commit_native(&context, reservation);
            }
        }
        Ok((reply, agent::task(context)))
    }

    fn running_reply<T>(
        &self,
        conversation: &str,
        reply: u32,
        apply: impl FnOnce(&mut Running) -> T,
    ) -> Option<T> {
        let mut state = self.state();
        let running = state
            .running
            .get_mut(conversation)
            .filter(|running| running.reply == reply)?;
        Some(apply(running))
    }

    /// Stops the Space's running reply.
    pub fn stop(&self, space: &str) -> bool {
        let state = self.state();
        let Some(id) = state.current.get(space) else {
            return false;
        };
        if let Some(preparing) = state.preparing.get(id) {
            preparing.cancel.cancel(CancelReason::Stopped);
            return true;
        }
        let Some(running) = state.running.get(id) else {
            return false;
        };
        running.cancel.cancel(CancelReason::Stopped);
        true
    }

    /// Brings cite `n` of a reply into view on its page, while that page is
    /// the document the reply read.
    pub fn cite(&self, conversation: &str, reply: u32, n: u8) -> bool {
        let jump = {
            let state = self.state();
            let Some(data) = state
                .live
                .get(conversation)
                .and_then(|live| live.replies.get(&reply))
            else {
                return false;
            };
            data.book
                .jump(n)
                .map(|jump| (jump.tab.to_owned(), jump.epoch, jump.highlight.to_owned()))
        };
        let Some((tab, epoch, highlight)) = jump else {
            return false;
        };
        self.ports()
            .page
            .facts(&tab)
            .is_some_and(|facts| facts.epoch == epoch)
            && self.ports().page.highlight(&tab, epoch, &highlight)
    }

    /// Emphasises item `n` of an items block on its page; none clears it.
    pub fn focus(&self, conversation: &str, reply: u32, block: u16, n: Option<u8>) -> bool {
        let owner = {
            let state = self.state();
            state
                .marks
                .iter()
                .find(|(_, (c, r, b))| c == conversation && *r == reply && *b == block)
                .map(|(tab, _)| tab.clone())
        };
        let Some(tab) = owner else {
            return false;
        };
        let Some(facts) = self.ports().page.facts(&tab) else {
            return false;
        };
        self.ports()
            .page
            .mark_focus(&tab, facts.epoch, n, n.is_some());
        true
    }

    /// The person's answer to a waiting confirm, a proposed style or an
    /// offered handoff.
    pub fn decide(
        &self,
        conversation: &str,
        reply: u32,
        block: u16,
        decision: PageAssistDecisionV1,
    ) -> bool {
        let style = {
            let mut state = self.state();
            state
                .live
                .get_mut(conversation)
                .and_then(|live| live.replies.get_mut(&reply))
                .and_then(|data| data.styles.remove(&block))
        };
        if let Some(style) = style {
            let state = match decision {
                PageAssistDecisionV1::Approve => {
                    match self.add_style(&style.profile, &style.site, &style.summary, style.css) {
                        Some(id) => (PageStyleStateV1::Applied, Some(id)),
                        None => (PageStyleStateV1::Full, None),
                    }
                }
                PageAssistDecisionV1::Decline => (PageStyleStateV1::Declined, None),
            };
            let value = PageAssistBlockV1::Style {
                style: PageStyleProposalV1 {
                    id: state.1,
                    summary: style.summary,
                    site: style.site,
                    rules: style.rules,
                    state: state.0,
                },
            };
            self.apply(
                conversation,
                reply,
                PageAssistEventV1::Block { block, value },
            );
            self.persist(conversation);
            return true;
        }
        if decision == PageAssistDecisionV1::Decline
            && self.dismiss_handoff(conversation, reply, block)
        {
            return true;
        }
        self.running_reply(conversation, reply, |running| {
            match running.decision.take() {
                Some((at, sender)) if at == block => Some(sender),
                other => {
                    running.decision = other;
                    None
                }
            }
        })
        .flatten()
        .is_some_and(|sender| sender.send(decision).is_ok())
    }

    fn handoff_block(
        &self,
        conversation: &str,
        reply: u32,
        block: u16,
    ) -> Option<PageAssistHandoffV1> {
        let state = self.state();
        match state
            .live
            .get(conversation)?
            .conversation
            .blocks(reply)?
            .get(usize::from(block))?
        {
            PageAssistBlockV1::Handoff { handoff } => Some(handoff.clone()),
            _ => None,
        }
    }

    fn set_handoff(
        &self,
        conversation: &str,
        reply: u32,
        block: u16,
        state: PageAssistHandoffStateV1,
    ) {
        if let Some(mut handoff) = self.handoff_block(conversation, reply, block) {
            handoff.state = state;
            self.apply(
                conversation,
                reply,
                PageAssistEventV1::Block {
                    block,
                    value: PageAssistBlockV1::Handoff { handoff },
                },
            );
            self.persist(conversation);
        }
    }

    fn dismiss_handoff(&self, conversation: &str, reply: u32, block: u16) -> bool {
        let start = {
            let mut state = self.state();
            state
                .live
                .get_mut(conversation)
                .and_then(|live| live.replies.get_mut(&reply))
                .and_then(|data| data.handoffs.remove(&block))
        };
        if start.is_none() {
            return false;
        }
        self.set_handoff(
            conversation,
            reply,
            block,
            PageAssistHandoffStateV1::Dismissed,
        );
        true
    }

    /// Starts the Work a reply offered.
    pub async fn continue_work(
        &self,
        conversation: &str,
        reply: u32,
        block: u16,
    ) -> PageAssistContinueV1 {
        let failed = |fault| PageAssistContinueV1 {
            work: None,
            fault: Some(fault),
        };
        let taken = {
            let mut state = self.state();
            state.live.get_mut(conversation).and_then(|live| {
                let start = live.replies.get_mut(&reply)?.handoffs.remove(&block)?;
                Some((live.profile, live.conversation.space().to_owned(), start))
            })
        };
        let Some((profile, space, start)) = taken else {
            return failed(PageAssistFaultV1::Invalid);
        };
        if !self.ports().settings.ai_enabled(&profile) {
            return failed(PageAssistFaultV1::AiOff);
        }
        self.set_handoff(
            conversation,
            reply,
            block,
            PageAssistHandoffStateV1::Starting,
        );
        match self.ports().work.begin(&profile, &space, start).await {
            Some(work) => {
                self.set_handoff(
                    conversation,
                    reply,
                    block,
                    PageAssistHandoffStateV1::Started { work: work.clone() },
                );
                PageAssistContinueV1 {
                    work: Some(work),
                    fault: None,
                }
            }
            None => {
                let fault = PageAssistFaultV1::Unavailable;
                self.set_handoff(
                    conversation,
                    reply,
                    block,
                    PageAssistHandoffStateV1::Failed { fault },
                );
                failed(fault)
            }
        }
    }

    /// Continues a paused reply; it reads the page again first.
    pub fn resume(&self, conversation: &str, reply: u32) -> bool {
        self.running_reply(conversation, reply, |running| running.resume.take())
            .flatten()
            .is_some_and(|sender| sender.send(()).is_ok())
    }

    fn release_marks(&self, conversation: &str) -> Vec<String> {
        let owned: Vec<(String, u32, u16)> = {
            let mut state = self.state();
            let tabs: Vec<String> = state
                .marks
                .iter()
                .filter(|(_, (c, _, _))| c == conversation)
                .map(|(tab, _)| tab.clone())
                .collect();
            tabs.iter()
                .filter_map(|tab| state.marks.remove(tab).map(|(_, r, b)| (tab.clone(), r, b)))
                .collect()
        };
        for (tab, reply, block) in &owned {
            self.ports().page.marks_clear(tab);
            self.unmark(conversation, *reply, *block);
        }
        owned.into_iter().map(|(tab, _, _)| tab).collect()
    }

    /// Shows an items block's cards as no longer marked on the page.
    fn unmark(&self, conversation: &str, reply: u32, block: u16) {
        let value = {
            let state = self.state();
            match state
                .live
                .get(conversation)
                .and_then(|live| live.conversation.blocks(reply))
                .and_then(|blocks| blocks.get(usize::from(block)))
            {
                Some(PageAssistBlockV1::Items { tab, items }) if items.iter().any(|i| i.marked) => {
                    Some(PageAssistBlockV1::Items {
                        tab: tab.clone(),
                        items: items
                            .iter()
                            .cloned()
                            .map(|item| PageAssistItemV1 {
                                marked: false,
                                ..item
                            })
                            .collect(),
                    })
                }
                _ => None,
            }
        };
        if let Some(value) = value {
            self.apply(
                conversation,
                reply,
                PageAssistEventV1::Block { block, value },
            );
        }
    }

    /// The panel closed for `space`: its marks and picks leave the pages.
    pub fn close(&self, space: &str) {
        let current = self.state().current.get(space).cloned();
        if let Some(conversation) = current {
            if let Some(preparing) = self.state().preparing.get(&conversation) {
                preparing.cancel.cancel(CancelReason::Gone);
            }
            self.release_marks(&conversation);
        }
        let picks: Vec<(String, u64)> = self.state().picks.drain().map(|(key, _)| key).collect();
        for (tab, pick) in picks {
            self.ports().page.pick_cancel(&tab);
            self.ports().page.pick_clear(&tab, pick);
        }
    }

    fn leave(&self, space: &str) {
        let current = {
            let mut state = self.state();
            state.current_epochs.remove(space);
            state.current.remove(space)
        };
        if let Some(conversation) = current {
            if let Some(preparing) = self.state().preparing.remove(&conversation) {
                preparing.cancel.cancel(CancelReason::Gone);
            }
            if let Some(running) = self.state().running.remove(&conversation) {
                running.cancel.cancel(CancelReason::Gone);
            }
            self.release_marks(&conversation);
            self.persist(&conversation);
        }
    }

    /// Starts a new conversation in `space`; the old one stays in history.
    pub fn new_conversation(&self, space: &str) -> bool {
        let profile = {
            let state = self.state();
            state
                .current
                .get(space)
                .and_then(|id| state.live.get(id))
                .map(|live| (live.profile, live.conversation.is_empty()))
        };
        match profile {
            Some((_, true)) => {
                let state = self.state();
                if let Some(preparing) = state
                    .current
                    .get(space)
                    .and_then(|id| state.preparing.get(id))
                {
                    preparing.cancel.cancel(CancelReason::Gone);
                }
                return true;
            }
            Some(_) => {}
            None => return false,
        }
        self.leave(space);
        true
    }

    /// Past conversations of the profile `space` belongs to.
    pub fn history(&self, space: &str) -> PageAssistHistoryV1 {
        let conversations = self
            .ports()
            .tabs
            .profile(space)
            .map(|profile| {
                self.ports()
                    .conversations
                    .list(&profile, HISTORY)
                    .into_iter()
                    .map(|stored| PageAssistPastV1 {
                        id: stored.id,
                        title: stored.title,
                        space: stored.space,
                        updated_ms: stored.updated_ms as f64,
                    })
                    .collect()
            })
            .unwrap_or_default();
        PageAssistHistoryV1 {
            version: VERSION,
            conversations,
        }
    }

    /// Shows a past conversation in `space`.
    pub fn open(&self, space: &str, conversation: &str) -> bool {
        let Some(profile) = self.ports().tabs.profile(space) else {
            return false;
        };
        if self.state().current.get(space).map(String::as_str) == Some(conversation) {
            return true;
        }
        let loaded = self.state().live.contains_key(conversation);
        if !loaded {
            let Some(view) = self
                .ports()
                .conversations
                .load(&profile, conversation)
                .and_then(|body| serde_json::from_str::<PageAssistConversationV1>(&body).ok())
                .filter(|view| view.id == conversation)
            else {
                return false;
            };
            let mut state = self.state();
            state.live.insert(
                conversation.to_owned(),
                Live {
                    profile,
                    conversation: Conversation::restore(view, space.to_owned()),
                    revision: Arc::new(()),
                    replies: HashMap::new(),
                    links: HashSet::new(),
                    touched: 0,
                },
            );
        }
        self.leave(space);
        let mut state = self.state();
        if state
            .live
            .get(conversation)
            .is_none_or(|live| live.profile != profile)
        {
            return false;
        }
        state
            .current
            .insert(space.to_owned(), conversation.to_owned());
        state.current_epochs.insert(space.to_owned(), Arc::new(()));
        if let Some(live) = state.live.get_mut(conversation) {
            live.touched = self.ports().clock.now_ms();
        }
        Self::evict(&mut state);
        true
    }

    /// Deletes a conversation; when it is shown, the Space starts afresh.
    pub fn delete(&self, space: &str, conversation: &str) -> bool {
        let Some(profile) = self.ports().tabs.profile(space) else {
            return false;
        };
        if self.state().current.get(space).map(String::as_str) == Some(conversation) {
            self.leave(space);
        }
        self.state().live.remove(conversation);
        self.ports().conversations.delete(&profile, conversation)
    }

    fn persist(&self, conversation: &str) {
        let stored = {
            let state = self.state();
            let Some(live) = state.live.get(conversation) else {
                return;
            };
            if live.conversation.is_empty() {
                return;
            }
            let view = live.conversation.view();
            let Ok(body) = serde_json::to_string(view) else {
                return;
            };
            (
                live.profile,
                StoredConversation {
                    id: view.id.clone(),
                    space: live.conversation.space().to_owned(),
                    title: view.title.clone().unwrap_or_default(),
                    updated_ms: self.ports().clock.unix_ms(),
                },
                body,
            )
        };
        let (profile, entry, body) = stored;
        self.ports().conversations.save(&profile, &entry, body);
    }

    /// The person starts picking an element on `tab`.
    pub async fn pick(&self, space: &str, tab: &str) -> PageAssistPickResponseV1 {
        let failed = |fault| PageAssistPickResponseV1 {
            pick: None,
            fault: Some(fault),
        };
        let Some(facts) = self.ports().page.facts(tab) else {
            return failed(PageAssistFaultV1::Unavailable);
        };
        if self.ports().tabs.profile(space) != Some(facts.profile)
            || facts.space.as_deref().is_some_and(|s| s != space)
        {
            return failed(PageAssistFaultV1::Invalid);
        }
        let availability = self.draft_availability(Some(&facts)).await;
        if availability != PageAssistAvailabilityV1::Dormant {
            if let Some(fault) = policy::refusal(availability) {
                return failed(fault);
            }
        }
        if !self.draft_scope_current(space, tab, &facts) {
            return failed(PageAssistFaultV1::PageChanged);
        }
        match self.ports().page.pick(tab, facts.epoch).await {
            Ok(pick) => {
                if !self.draft_scope_current(space, tab, &facts) {
                    self.ports().page.pick_clear(tab, pick.pick);
                    return failed(PageAssistFaultV1::PageChanged);
                }
                let host = host_of(facts.url.as_deref().unwrap_or_default());
                let label = text::bounded(&text::clean(&pick.label), 64);
                let label = if label.is_empty() {
                    "Section".to_owned()
                } else {
                    label
                };
                let dropped: Vec<u64> = {
                    let mut state = self.state();
                    state.picks.insert(
                        (tab.to_owned(), pick.pick),
                        PickRecord {
                            epoch: facts.epoch,
                            label: label.clone(),
                        },
                    );
                    let mut mine: Vec<u64> = state
                        .picks
                        .keys()
                        .filter(|(t, _)| t == tab)
                        .map(|(_, id)| *id)
                        .collect();
                    mine.sort_unstable();
                    let excess = mine.len().saturating_sub(MAX_PICKS);
                    let dropped: Vec<u64> = mine.into_iter().take(excess).collect();
                    for id in &dropped {
                        state.picks.remove(&(tab.to_owned(), *id));
                    }
                    dropped
                };
                for id in dropped {
                    self.ports().page.pick_clear(tab, id);
                }
                PageAssistPickResponseV1 {
                    pick: Some(PageAssistPickV1 {
                        tab: tab.to_owned(),
                        pick: pick.pick.to_string(),
                        label,
                        host,
                    }),
                    fault: None,
                }
            }
            Err(PickFailure::Cancelled) => PageAssistPickResponseV1 {
                pick: None,
                fault: None,
            },
            Err(PickFailure::PageChanged) => failed(PageAssistFaultV1::PageChanged),
            Err(PickFailure::Unavailable) => failed(PageAssistFaultV1::Unavailable),
        }
    }

    /// Ends a pick in progress.
    pub fn pick_cancel(&self, tab: &str) {
        self.ports().page.pick_cancel(tab);
    }

    /// Detaches a picked element.
    pub fn detach(&self, tab: &str, pick: &str) -> bool {
        let Ok(id) = pick.parse::<u64>() else {
            return false;
        };
        let known = self.state().picks.remove(&(tab.to_owned(), id)).is_some();
        if known {
            self.ports().page.pick_clear(tab, id);
        }
        known
    }

    /// The tab committed a navigation: its marks and picks are gone, and
    /// replies that read it no longer jump to it.
    pub fn navigated(&self, tab: &str, _url: Option<&str>) {
        let (owner, inert) = {
            let mut state = self.state();
            state.traits.remove(tab);
            state.picks.retain(|(t, _), _| t != tab);
            let owner = state.marks.remove(tab);
            let inert: Vec<(String, u32)> = state
                .live
                .iter()
                .filter_map(|(id, live)| {
                    live.replies
                        .iter()
                        .filter(|(_, data)| data.reads.iter().any(|read| read.tab == tab))
                        .map(|(reply, _)| *reply)
                        .max()
                        .map(|reply| (id.clone(), reply))
                })
                .collect();
            (owner, inert)
        };
        if let Some((conversation, reply, block)) = owner {
            self.unmark(&conversation, reply, block);
        }
        for (conversation, reply) in inert {
            self.apply(&conversation, reply, PageAssistEventV1::Inert);
        }
    }

    /// The tab closed.
    pub fn tab_closed(&self, tab: &str) {
        let mut state = self.state();
        state.traits.remove(tab);
        state.picks.retain(|(t, _), _| t != tab);
        state.marks.remove(tab);
    }

    /// The person used the tab: steps running on it pause.
    pub fn human_input(&self, tab: &str) {
        let state = self.state();
        for running in state.running.values() {
            if running.acting.as_deref() == Some(tab) && running.leased.load(Ordering::Acquire) {
                running.took_over.store(true, Ordering::Release);
            }
        }
    }

    /// Ends every conversation in memory: AI off, profile switch or quit.
    pub fn end_all(&self) {
        let (events, tabs) = {
            let mut state = self.state();
            for running in state.running.values() {
                running.cancel.cancel(CancelReason::Gone);
            }
            state.running.clear();
            for preparing in state.preparing.values() {
                preparing.cancel.cancel(CancelReason::Gone);
            }
            state.preparing.clear();
            let events: Vec<PageAssistChangedV1> = state
                .current
                .values()
                .filter_map(|id| state.live.get(id))
                .map(|live| {
                    self.conversation_wide(&live.conversation, PageAssistEventV1::Invalidated)
                })
                .collect();
            let tabs: Vec<String> = state.marks.drain().map(|(tab, _)| tab).collect();
            state.picks.clear();
            state.current.clear();
            state.current_epochs.clear();
            state.live.clear();
            state.traits.clear();
            (events, tabs)
        };
        for tab in tabs {
            self.ports().page.marks_clear(&tab);
        }
        self.emit_all(events);
    }

    /// A hosted call of a reply is confirmed; the reply's charge adds up.
    pub fn settle_hosted(&self, conversation: &str, reply: u32, credits: String) {
        let cost = (|| {
            let mut state = self.state();
            let live = state.live.get_mut(conversation)?;
            let model = live
                .conversation
                .messages()
                .iter()
                .find_map(|message| match message {
                    PageAssistMessageV1::Reply {
                        id,
                        cost: Some(PageAssistCostV1::Hosted { model, .. }),
                        ..
                    } if *id == reply => Some(model.clone()),
                    _ => None,
                });
            let data = live.replies.get_mut(&reply)?;
            let total = zephium_page_assist::cost::add_credits(data.credits.as_deref(), &credits)?;
            data.credits = Some(total.clone());
            model.map(|model| PageAssistCostV1::Hosted {
                model,
                credits: Some(total),
            })
        })();
        if let Some(cost) = cost {
            self.apply(conversation, reply, PageAssistEventV1::Cost { cost });
        }
    }

    // ------------------------------------------------------------ page styles

    fn styles_of(&self, profile: &ProfileId) -> Vec<StoredStyle> {
        let mut state = self.state();
        if !state.styles.contains_key(profile) {
            let loaded = self.ports().styles.load(profile);
            state.styles.insert(*profile, loaded);
        }
        state.styles.get(profile).cloned().unwrap_or_default()
    }

    fn apply_site(&self, profile: &ProfileId, site: &str, styles: &[StoredStyle]) {
        let css: String = styles
            .iter()
            .filter(|style| style.enabled && style.site == site)
            .map(|style| style.css.as_str())
            .collect();
        self.ports().page.apply_styles(profile, site, &css);
    }

    fn change_styles(
        &self,
        profile: &ProfileId,
        site: &str,
        change: impl FnOnce(&mut Vec<StoredStyle>) -> bool,
    ) -> bool {
        let mut styles = self.styles_of(profile);
        if !change(&mut styles) || !self.ports().styles.save(profile, &styles) {
            return false;
        }
        self.apply_site(profile, site, &styles);
        self.state().styles.insert(*profile, styles);
        true
    }

    /// Whether `site` can take one more style of `bytes` CSS.
    pub(crate) fn site_has_room(&self, profile: &ProfileId, site: &str, bytes: usize) -> bool {
        let styles = self.styles_of(profile);
        let (count, size) = styles
            .iter()
            .filter(|style| style.site == site)
            .fold((0, 0), |(count, size), style| {
                (count + 1, size + style.css.len())
            });
        count < MAX_SITE_STYLES && size + bytes <= MAX_SITE_CSS_BYTES
    }

    /// Stores and applies a validated style; none when the site is full or
    /// it could not be stored.
    pub(crate) fn add_style(
        &self,
        profile: &ProfileId,
        site: &str,
        summary: &str,
        css: String,
    ) -> Option<String> {
        if !self.site_has_room(profile, site, css.len()) {
            return None;
        }
        let id = {
            let mut state = self.state();
            state.next_style += 1;
            format!("s{}-{}", self.ports().clock.now_ms(), state.next_style)
        };
        let style = StoredStyle {
            id: id.clone(),
            site: site.to_owned(),
            summary: summary.to_owned(),
            css,
            enabled: true,
        };
        self.change_styles(profile, site, |styles| {
            styles.push(style);
            true
        })
        .then_some(id)
    }

    fn site_styles(&self, profile: &ProfileId, site: &str) -> Vec<PageStyleV1> {
        self.styles_of(profile)
            .into_iter()
            .filter(|style| style.site == site)
            .map(|style| PageStyleV1 {
                id: style.id,
                summary: style.summary,
                enabled: style.enabled,
            })
            .collect()
    }

    fn tab_site(&self, tab: &str) -> Option<(ProfileId, String)> {
        let facts = self.ports().page.facts(tab)?;
        Some((facts.profile, site_of(facts.url.as_deref())?))
    }

    /// The styles of the site in `tab`.
    pub fn styles(&self, tab: &str) -> PageStylesV1 {
        let (site, styles) = match self.tab_site(tab) {
            Some((profile, site)) => {
                let styles = self.site_styles(&profile, &site);
                (Some(site), styles)
            }
            None => (None, Vec::new()),
        };
        PageStylesV1 {
            version: VERSION,
            tab: tab.to_owned(),
            site,
            styles,
        }
    }

    /// Turns one of the tab's site styles on or off.
    pub fn toggle_style(&self, tab: &str, style: &str, enabled: bool) -> PageStylesV1 {
        if let Some((profile, site)) = self.tab_site(tab) {
            self.set_style(&profile, &site, style, Some(enabled));
        }
        self.styles(tab)
    }

    /// Deletes one of the tab's site styles.
    pub fn remove_style(&self, tab: &str, style: &str) -> PageStylesV1 {
        if let Some((profile, site)) = self.tab_site(tab) {
            self.set_style(&profile, &site, style, None);
        }
        self.styles(tab)
    }

    fn set_style(&self, profile: &ProfileId, site: &str, id: &str, enabled: Option<bool>) -> bool {
        self.change_styles(profile, site, |styles| {
            let Some(index) = styles
                .iter()
                .position(|style| style.id == id && style.site == site)
            else {
                return false;
            };
            match enabled {
                Some(enabled) => styles[index].enabled = enabled,
                None => {
                    styles.remove(index);
                }
            }
            true
        })
    }

    /// Every site's styles in `profile`, for Settings.
    pub fn all_styles(&self, profile: &ProfileId) -> PageStylesAllV1 {
        let mut sites: Vec<PageSiteStylesV1> = Vec::new();
        for style in self.styles_of(profile) {
            let row = PageStyleV1 {
                id: style.id,
                summary: style.summary,
                enabled: style.enabled,
            };
            match sites.iter_mut().find(|site| site.site == style.site) {
                Some(site) => site.styles.push(row),
                None => sites.push(PageSiteStylesV1 {
                    site: style.site,
                    styles: vec![row],
                }),
            }
        }
        sites.sort_by(|a, b| a.site.cmp(&b.site));
        PageStylesAllV1 {
            version: VERSION,
            profile: profile.to_string(),
            sites,
        }
    }

    /// Turns a site's style on or off from Settings.
    pub fn toggle_style_for(
        &self,
        profile: &ProfileId,
        site: &str,
        style: &str,
        enabled: bool,
    ) -> PageStylesAllV1 {
        self.set_style(profile, site, style, Some(enabled));
        self.all_styles(profile)
    }

    /// Deletes a site's style from Settings.
    pub fn remove_style_for(
        &self,
        profile: &ProfileId,
        site: &str,
        style: &str,
    ) -> PageStylesAllV1 {
        self.set_style(profile, site, style, None);
        self.all_styles(profile)
    }

    /// The enabled styles of `site`, for a page that just loaded.
    pub fn site_css(&self, profile: &ProfileId, site: &str) -> String {
        self.styles_of(profile)
            .iter()
            .filter(|style| style.enabled && style.site == site)
            .map(|style| style.css.as_str())
            .collect()
    }
}
