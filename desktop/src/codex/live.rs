//! One native worker owns each profile's original process and all pending calls.
use super::admission::{self, Settling};
use super::delivery::ReplyDelivery;
#[path = "browser.rs"]
mod browser;
#[path = "browser_reservation.rs"]
mod browser_reservation;
#[path = "browser_worker.rs"]
mod browser_worker;
#[path = "removal.rs"]
mod removal;
use super::*;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex, OnceLock, RwLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use zephium_codex_broker::{
    catalog::{Catalog, Effort, EffortSelection},
    connection::{
        AccountState, Connection, ConversationKey, Event, Fault, LoginUrl, TextAdmission,
        TextRequest,
    },
    conversation::{self},
    profile::ProfileId as NativeProfile,
};
use zephium_core::{ids::ProfileId, page_choice, ports::store::Store as _};
use zephium_ipc::page_assist::{PageAssistModelSourceV1, PageAssistNativeFaultV1};
use zephium_work_composition::page_assist::port::*;

static STOPPED: AtomicBool = AtomicBool::new(false);
static REGISTRY: OnceLock<Arc<Registry>> = OnceLock::new();
struct Registry {
    app: tauri::AppHandle,
    services: Mutex<HashMap<ProfileId, Arc<Service>>>,
    foreground: RwLock<Option<String>>,
    epoch: AtomicU64,
    closed: AtomicBool,
    removals: removal::Inventory,
}
struct Service {
    profile: ProfileId,
    epoch: u64,
    stop: AtomicBool,
    done: AtomicBool,
    view: Mutex<CodexViewV1>,
    login: Mutex<Option<LoginAttempt>>,
    account_revision: Arc<AtomicU64>,
    selection_revision: AtomicU64,
    queue: admission::Queue<Command>,
    settling: Mutex<Option<Settling>>,
    browser_quarantine: Mutex<Vec<browser_worker::Quarantine>>,
    join: Mutex<Option<JoinHandle<()>>>,
    removal: Mutex<Option<Arc<removal::Operation>>>,
}
struct LoginAttempt {
    id: String,
    cancel: Arc<AtomicBool>,
}
enum Command {
    Login(LoginAttempt),
    CancelLogin(String),
    Logout,
    Models,
    Quota,
    Removal(Arc<removal::Operation>),
    RemovalLocal(removal::LocalRequest),
    Select(Box<SelectionCall>),
    PrepareBrowser(Box<browser_reservation::Prepare>),
    Reply(ReplyCall),
}
struct SelectionCall {
    expected: page_choice::Snapshot,
    selection: page_choice::Selection,
    deadline: Instant,
    result: tokio::sync::oneshot::Sender<Result<page_choice::Reply, CodexFaultV1>>,
}
struct ReplyCall {
    turn: Option<admission::Turn>,
    account_revision: u64,
    request: NativeConversationRequest,
    selection: CodexSelectionV1,
    events: NativeConversationEvents,
    cancel: Arc<AtomicBool>,
    result: ReplySink,
    browser: Option<Box<browser_reservation::Start>>,
}
/// Text and Browser completion contracts cannot be silently interchanged.
/// Only the exact dual receipt or explicit quarantine path constructs Browser
/// completion; a provider terminal alone remains retained in Active.
enum ReplySink {
    Text(tokio::sync::oneshot::Sender<ReplyDelivery<NativeConversationOutcome>>),
    Browser(tokio::sync::oneshot::Sender<ReplyDelivery<NativeBrowserCompletion>>),
}
impl ReplySink {
    fn send(self, value: ReplyDelivery<NativeConversationOutcome>) -> Result<(), ()> {
        match self {
            Self::Text(sender) => sender.send(value).map_err(|_| ()),
            Self::Browser(sender) => {
                let fault = match value {
                    ReplyDelivery::Failed(fault) => fault,
                    ReplyDelivery::Settled(_) => PageAssistNativeFaultV1::Protocol,
                };
                sender.send(ReplyDelivery::Failed(fault)).map_err(|_| ())
            }
        }
    }
    fn browser(self, value: NativeBrowserCompletion) {
        match self {
            Self::Browser(sender) => {
                let _ = sender.send(ReplyDelivery::Settled(value));
            }
            Self::Text(sender) => {
                let _ = sender.send(ReplyDelivery::Failed(PageAssistNativeFaultV1::Protocol));
            }
        }
    }
}

struct Active {
    browser: bool,
    delivery: Option<ReplyDelivery<NativeConversationOutcome>>,
    call: ReplyCall,
    items: HashMap<String, String>,
    cancel_sent: bool,
}
impl Registry {
    fn scope(&self, expected: &str) -> Option<u64> {
        let foreground = self.foreground.read().ok()?;
        (!self.closed.load(Ordering::Acquire) && foreground.as_deref() == Some(expected))
            .then(|| self.epoch.load(Ordering::Acquire))
    }
    fn current(&self, service: &Service) -> bool {
        if !self.removals.admission_open() || self.closed.load(Ordering::Acquire) {
            return false;
        }
        if let Some(operation) = service.removal.lock().ok().and_then(|value| value.clone()) {
            return operation.current()
                && !service.stop.load(Ordering::Acquire)
                && !service.done.load(Ordering::Acquire);
        }
        !service.stop.load(Ordering::Acquire)
            && !service.done.load(Ordering::Acquire)
            && self.scope(&service.profile.to_string()) == Some(service.epoch)
    }
    fn service(
        self: &Arc<Self>,
        profile: ProfileId,
        epoch: u64,
        retry_completed: bool,
    ) -> Result<Arc<Service>, CodexFaultV1> {
        if !self.removals.admission_open() || self.removals.contains(profile) {
            return Err(CodexFaultV1::RecoveryRequired);
        }
        let mut services = self
            .services
            .lock()
            .map_err(|_| CodexFaultV1::Unavailable)?;
        if self.scope(&profile.to_string()) != Some(epoch) {
            return Err(CodexFaultV1::ScopeChanged);
        }
        if !self.removals.admission_open() || self.removals.contains(profile) {
            return Err(CodexFaultV1::RecoveryRequired);
        }
        if let Some(service) = services.get(&profile) {
            if service.epoch == epoch && (!service.done.load(Ordering::Acquire) || !retry_completed)
            {
                return Ok(service.clone());
            }
            if !service.fully_settled() {
                return Err(CodexFaultV1::Busy);
            }
        }
        services.retain(|_, service| !service.fully_settled());
        if services.len() >= 64 {
            return Err(CodexFaultV1::Busy);
        }
        self.spawn_service(&mut services, profile, epoch, None, false)
    }
    fn spawn_service(
        self: &Arc<Self>,
        services: &mut HashMap<ProfileId, Arc<Service>>,
        profile: ProfileId,
        epoch: u64,
        removal: Option<Arc<removal::Operation>>,
        local_only: bool,
    ) -> Result<Arc<Service>, CodexFaultV1> {
        let (queue, receiver) = admission::channel(32);
        let mut view = CodexViewV1::unavailable(profile.to_string());
        view.state = CodexStateV1::Preparing;
        view.error = None;
        view.page_selected = page_requested(profile);
        let service = Arc::new(Service {
            profile,
            epoch,
            stop: AtomicBool::new(false),
            done: AtomicBool::new(false),
            view: Mutex::new(view),
            login: Mutex::new(None),
            account_revision: Arc::new(AtomicU64::new(1)),
            selection_revision: AtomicU64::new(u64::MAX),
            queue,
            settling: Mutex::new(None),
            browser_quarantine: Mutex::new(Vec::new()),
            join: Mutex::new(None),
            removal: Mutex::new(removal),
        });
        let owned = service.clone();
        let registry = Arc::downgrade(self);
        let join = std::thread::Builder::new()
            .name("codex-profile".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if local_only {
                        removal::local_worker(&owned, &registry, receiver)
                    } else {
                        worker(&owned, &registry, receiver)
                    }
                }));
                worker_returned(&owned, result.is_err());
                notify_models(&owned, &registry);
            })
            .map_err(|_| CodexFaultV1::Unavailable)?;
        *service
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(join);
        services.insert(profile, service.clone());
        Ok(service)
    }
}
/// Called only after the owned worker returned or unwound. Original physical
/// teardown is not proof that a committed Browser Page effect returned.
fn worker_returned(service: &Service, panicked: bool) {
    if panicked {
        set_fault(
            service,
            if service.queue.browser_occupied() {
                CodexFaultV1::RecoveryRequired
            } else {
                CodexFaultV1::Unavailable
            },
        );
    }
    admission::release_text_after_close(
        &mut service.settling.lock().unwrap_or_else(|e| e.into_inner()),
    );
    service.done.store(true, Ordering::Release);
}

/// The same transfer runs in both the ordinary and retired queue drains. It
/// never discards a different held turn or reconstructs a token from a reply ID.
fn adopt_reply_turn(service: &Service, call: &mut ReplyCall) -> bool {
    let mut held = service.settling.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_some() {
        return false;
    }
    *held = call
        .turn
        .take()
        .map(|turn| Settling::new(call.request.conversation.clone(), call.request.reply, turn));
    held.is_some()
}

/// Browser presentation waits for the exact dual settlement. Page closes its
/// request receiver and supplies local receipt independently, so this cannot
/// deadlock entered effects. Text retains its existing terminal delivery.
fn deliver_outcome(active: &mut Option<Active>, outcome: NativeConversationOutcome) {
    if let Some(current) = active.as_mut().filter(|current| current.browser) {
        if !matches!(current.delivery, Some(ReplyDelivery::Settled(_))) {
            current.delivery = Some(ReplyDelivery::Settled(outcome));
        }
    } else if let Some(current) = active.take() {
        let _ = current.call.result.send(ReplyDelivery::Settled(outcome));
    }
}

fn deliver_failure(active: &mut Option<Active>, fault: PageAssistNativeFaultV1) {
    if let Some(current) = active.as_mut().filter(|current| current.browser) {
        if current.delivery.is_none() {
            current.delivery = Some(ReplyDelivery::Failed(fault));
        }
    } else if let Some(current) = active.take() {
        let _ = current.call.result.send(ReplyDelivery::Failed(fault));
    }
}

impl Service {
    /// A finished worker is not an absent owner while a Browser local effect
    /// lacks its actual settlement receipt. A lost receipt quarantines the slot.
    fn fully_settled(&self) -> bool {
        self.done.load(Ordering::Acquire)
            && !self.queue.browser_occupied()
            && self
                .browser_quarantine
                .lock()
                .is_ok_and(|held| held.is_empty())
            && self.settling.lock().is_ok_and(|held| {
                held.as_ref()
                    .is_none_or(|value| !value.has_local_obligation())
            })
    }
    fn advance_revision(&self) -> bool {
        self.account_revision
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value != 0)
                    .then(|| value.checked_add(1))
                    .flatten()
                    .filter(|next| *next != u64::MAX)
            })
            .is_ok()
    }
    fn retire_ordinary(&self) {
        self.account_revision.store(0, Ordering::Release);
        self.queue.set_ready(false);
        if let Ok(login) = self.login.lock() {
            if let Some(login) = login.as_ref() {
                login.cancel.store(true, Ordering::Release);
            }
        }
    }
    fn retire_for_scope(&self) {
        if self.removal.lock().is_ok_and(|value| value.is_some()) {
            self.retire_ordinary();
        } else {
            self.request_stop();
        }
    }
    fn request_stop(&self) {
        // Zero is a terminal admission revision for this exact Service. No
        // later state notification may resurrect a queued writer's fence.
        self.account_revision.store(0, Ordering::Release);
        if let Ok(removal) = self.removal.lock() {
            if let Some(value) = removal.as_ref() {
                value.cancel.store(true, Ordering::Release);
            }
        }
        self.stop.store(true, Ordering::Release);
        self.queue.set_ready(false);
        if let Ok(login) = self.login.lock() {
            if let Some(login) = login.as_ref() {
                login.cancel.store(true, Ordering::Release);
            }
        }
    }
    fn snapshot(&self) -> CodexViewV1 {
        let mut view = self
            .view
            .lock()
            .map(|view| view.clone())
            .unwrap_or_else(|_| CodexViewV1::unavailable(self.profile.to_string()));
        // Retirement can occur while another caller holds the view lock. Do
        // not acquire that lock in request_stop; fence the returned observation
        // here even if no final quota notification can reach the owner.
        if self.stop.load(Ordering::Acquire)
            || self.done.load(Ordering::Acquire)
            || self.account_revision.load(Ordering::Acquire) == 0
        {
            view.quota = Default::default();
            view.text_only = true;
        }
        view
    }
    fn enqueue(&self, command: Command) -> Result<(), CodexFaultV1> {
        if self.stop.load(Ordering::Acquire) || self.done.load(Ordering::Acquire) {
            return Err(CodexFaultV1::ScopeChanged);
        }
        let removing = self.removal.lock().is_ok_and(|value| value.is_some());
        if removing
            && !matches!(
                &command,
                Command::Removal(_) | Command::RemovalLocal(_) | Command::CancelLogin(_)
            )
        {
            return Err(CodexFaultV1::ScopeChanged);
        }
        let revocation = matches!(
            &command,
            Command::Logout
                | Command::CancelLogin(_)
                | Command::Removal(_)
                | Command::RemovalLocal(_)
        );
        self.queue
            .enqueue(command, revocation)
            .map_err(|error| match error {
                admission::Error::Busy => CodexFaultV1::Busy,
                admission::Error::Closed => CodexFaultV1::ScopeChanged,
            })
    }
}
fn set_fault(service: &Service, fault: CodexFaultV1) {
    if let Ok(mut view) = service.view.lock() {
        view.error = Some(fault);
        view.state = if fault == CodexFaultV1::RecoveryRequired {
            CodexStateV1::RecoveryRequired
        } else {
            CodexStateV1::Unavailable
        };
        view.models.clear();
        view.text_only = true;
        view.login_attempt = None;
        view.quota = Default::default();
    }
}
fn fault(value: Fault) -> CodexFaultV1 {
    match value {
        Fault::ScopeChanged => CodexFaultV1::ScopeChanged,
        Fault::Busy => CodexFaultV1::Busy,
        Fault::UnsupportedRuntime => CodexFaultV1::RuntimeUnsupported,
        Fault::RuntimeUnavailable => CodexFaultV1::RuntimeMissing,
        Fault::Profile => CodexFaultV1::ProfileStorage,
        Fault::RecoveryRequired => CodexFaultV1::RecoveryRequired,
        Fault::Vault => CodexFaultV1::Vault,
        Fault::PersistenceUnavailable => CodexFaultV1::ProfileStorage,
        Fault::HistoryUncertain => CodexFaultV1::OutcomeUnknown,
        Fault::Cancelled => CodexFaultV1::Cancelled,
        Fault::Timeout => CodexFaultV1::Timeout,
        Fault::Protocol | Fault::Policy | Fault::UnsupportedAction | Fault::UnsupportedText => {
            CodexFaultV1::Protocol
        }
        _ => CodexFaultV1::Unavailable,
    }
}
fn page_fault(value: Fault) -> PageAssistNativeFaultV1 {
    match value {
        Fault::ScopeChanged => PageAssistNativeFaultV1::ScopeChanged,
        Fault::Busy => PageAssistNativeFaultV1::Unavailable,
        Fault::Timeout => PageAssistNativeFaultV1::Timeout,
        Fault::Cancelled => PageAssistNativeFaultV1::Cancelled,
        Fault::UnsupportedText => PageAssistNativeFaultV1::UnsupportedInput,
        Fault::HistoryUncertain => PageAssistNativeFaultV1::OutcomeUnknown,
        Fault::UnknownConversation | Fault::HistoryDiverged | Fault::HistoryPolicy => {
            PageAssistNativeFaultV1::HistoryDiverged
        }
        Fault::Limit => PageAssistNativeFaultV1::Limit,
        Fault::Transport => PageAssistNativeFaultV1::OutcomeUnknown,
        _ => PageAssistNativeFaultV1::Protocol,
    }
}
fn effort(value: CodexEffortV1) -> Effort {
    match value {
        CodexEffortV1::None => Effort::None,
        CodexEffortV1::Minimal => Effort::Minimal,
        CodexEffortV1::Low => Effort::Low,
        CodexEffortV1::Medium => Effort::Medium,
        CodexEffortV1::High => Effort::High,
        CodexEffortV1::XHigh => Effort::XHigh,
        CodexEffortV1::Max => Effort::Max,
    }
}
fn selected_effort(value: CodexEffortSelectionV1) -> EffortSelection {
    match value {
        CodexEffortSelectionV1::Default => EffortSelection::Default,
        CodexEffortSelectionV1::Explicit(value) => EffortSelection::Explicit(effort(value)),
    }
}
fn core_choice(value: CodexSelectionV1) -> page_choice::CodexChoice {
    page_choice::CodexChoice {
        model: value.model,
        effort: value.effort,
    }
}
fn view_choice(value: page_choice::CodexChoice) -> CodexSelectionV1 {
    CodexSelectionV1 {
        model: value.model,
        effort: value.effort,
    }
}
fn read_selection(profile: ProfileId) -> Result<page_choice::Snapshot, CodexFaultV1> {
    let store = APP_STORE.get().ok_or(CodexFaultV1::ProfileStorage)?;
    match store.page_choice_call(profile, page_choice::Call::Read) {
        page_choice::Reply::Loaded(value) if value.profile == profile && value.valid() => Ok(value),
        _ => Err(CodexFaultV1::ProfileStorage),
    }
}
fn observe_selection(
    service: &Service,
    snapshot: &page_choice::Snapshot,
    expected_account: Option<u64>,
) -> bool {
    if snapshot.profile != service.profile || !snapshot.valid() {
        return false;
    }
    let Ok(mut view) = service.view.lock() else {
        return false;
    };
    if expected_account
        .is_some_and(|expected| expected != service.account_revision.load(Ordering::Acquire))
    {
        return false;
    }
    let current_revision = service.selection_revision.load(Ordering::Acquire);
    if current_revision != u64::MAX && snapshot.revision < current_revision {
        return false;
    }
    let choice = snapshot.codex.clone().map(view_choice);
    let selected = snapshot.backend == page_choice::Backend::Codex;
    if service.selection_revision.load(Ordering::Acquire) != snapshot.revision
        || view.page_selected != selected
        || view.selection != choice
    {
        // A physically settled owner may still be retained for account
        // presentation. Its terminal admission revision must stay terminal,
        // but confirmed configuration must not remain falsely selected.
        if !service.done.load(Ordering::Acquire) && !service.advance_revision() {
            service.request_stop();
            return false;
        }
    }
    view.page_selected = selected;
    view.selection = choice;
    service
        .selection_revision
        .store(snapshot.revision, Ordering::Release);
    true
}
fn refresh_selection(service: &Service) {
    let revision = service.account_revision.load(Ordering::Acquire);
    match read_selection(service.profile) {
        Ok(snapshot) => {
            let _ = observe_selection(service, &snapshot, Some(revision));
        }
        Err(_) if service.account_revision.load(Ordering::Acquire) == revision => {
            selection_uncertain(service)
        }
        Err(_) => {}
    }
}
fn selection_uncertain(service: &Service) {
    service
        .selection_revision
        .store(u64::MAX, Ordering::Release);
    let _ = service.advance_revision();
    service.queue.set_ready(false);
    if let Ok(mut view) = service.view.lock() {
        view.error = Some(CodexFaultV1::OutcomeUnknown);
    }
}
fn project_effort(value: Effort) -> CodexEffortV1 {
    match value {
        Effort::None => CodexEffortV1::None,
        Effort::Minimal => CodexEffortV1::Minimal,
        Effort::Low => CodexEffortV1::Low,
        Effort::Medium => CodexEffortV1::Medium,
        Effort::High => CodexEffortV1::High,
        Effort::XHigh => CodexEffortV1::XHigh,
        Effort::Max => CodexEffortV1::Max,
    }
}
pub(super) fn page_requested(profile: ProfileId) -> bool {
    // Missing/corrupt/unconfirmed selection must not silently activate BYOK.
    !read_selection(profile).is_ok_and(|value| value.backend == page_choice::Backend::Model)
}
fn catalog(service: &Service, catalog: &Catalog) {
    if let Ok(mut view) = service.view.lock() {
        view.models = catalog
            .models()
            .iter()
            .map(|row| CodexModelV1 {
                id: row.id.clone(),
                name: row.display_name.clone(),
                efforts: row
                    .efforts
                    .iter()
                    .map(|e| project_effort(e.effort))
                    .collect(),
                default_effort: row.default_effort.map(project_effort),
            })
            .collect();
    }
    refresh_selection(service);
}

fn state(service: &Service, value: AccountState) {
    if let Ok(mut view) = service.view.lock() {
        if value == AccountState::Unknown {
            let _ = service.advance_revision();
        }
        view.state = match value {
            AccountState::ChatGpt => CodexStateV1::Connected,
            AccountState::SignedOut => CodexStateV1::SignedOut,
            AccountState::StartingLogin
            | AccountState::AwaitingBrowser
            | AccountState::Cancelling => CodexStateV1::SigningIn,
            AccountState::Checking | AccountState::Unknown | AccountState::Cancelled => {
                CodexStateV1::Preparing
            }
            _ => CodexStateV1::Unavailable,
        };
        if view.state != CodexStateV1::SigningIn {
            view.login_attempt = None;
        }
        if value != AccountState::ChatGpt {
            view.models.clear();
            view.quota = Default::default();
            if value == AccountState::SignedOut {
                view.quota.state = zephium_ipc::codex_quota::CodexQuotaStateV1::Unavailable;
                view.quota.unavailable =
                    Some(zephium_ipc::codex_quota::CodexQuotaUnavailableV1::SignedOut);
            }
        }
        view.error = None;
    }
}

fn quota(service: &Service, observation: zephium_codex_broker::connection::QuotaView) {
    if let Ok(mut view) = service.view.lock() {
        if view.state != CodexStateV1::Connected {
            return;
        }
        view.quota = super::quota::project(
            &observation.state,
            observation.profile_revision,
            observation.account_revision,
            observation.checking,
            observation.refresh_needed,
            observation.last_attempt_at_ms,
        );
    }
}
struct DurableAssociations {
    profile: ProfileId,
    store: Arc<zephium_store::SqliteStore>,
}
impl zephium_codex_broker::connection::Associations for DurableAssociations {
    fn call(&mut self, call: zephium_core::codex_store::Call) -> zephium_core::codex_store::Reply {
        self.store.codex_call(self.profile, call)
    }
}
fn worker(
    service: &Arc<Service>,
    registry: &std::sync::Weak<Registry>,
    receiver: admission::Receiver<Command>,
) {
    let Some(profile) = NativeProfile::from_native(service.profile.bytes()) else {
        set_fault(service, CodexFaultV1::Invalid);
        return;
    };
    let Some(store) = APP_STORE.get().cloned() else {
        set_fault(service, CodexFaultV1::ProfileStorage);
        return;
    };
    let persistence = Box::new(DurableAssociations {
        profile: service.profile,
        store,
    });
    // Public release metadata is a lookup optimization only. The broker always
    // reauthenticates official metadata and the complete held image, then probes
    // compatibility in a disposable home before opening this account profile.
    const RUNTIME_HINT: &str = "codex.runtime.verified-release.v1";
    let cached_release = APP_STORE
        .get()
        .and_then(|store| store.app_setting(RUNTIME_HINT));
    let removal_at_start = service.removal.lock().ok().and_then(|value| value.clone());
    let removal_admission = removal_at_start
        .as_ref()
        .and_then(|operation| operation.admission());
    let opened = if let Some(operation) = removal_at_start.as_ref() {
        Connection::open_for_removal(
            profile,
            service.epoch,
            persistence,
            cached_release.as_deref(),
            removal_admission
                .as_ref()
                .expect("native removal action exists"),
            operation.deadline,
        )
    } else {
        Connection::open(
            profile,
            service.epoch,
            persistence,
            cached_release.as_deref(),
            &service.stop,
        )
    };
    let mut connection = match opened {
        Ok(connection) => connection,
        Err(error) => {
            set_fault(service, fault(error));
            if let Some(operation) = removal_at_start {
                removal::open_failed(registry, &operation);
            }
            return;
        }
    };
    // This cache does not own account authority; inability to update it must
    // not turn a healthy original account into a storage/authentication error.
    if let (Some(store), Some(version)) = (APP_STORE.get(), connection.release_version()) {
        let _ = store.set_app_setting(RUNTIME_HINT.into(), version.to_owned());
    }
    let scope = connection.scope().clone();
    let mut active: Option<Active> = None;
    let mut rejected_browser = None;
    let mut retired_command = None;
    let mut removal_active: Option<Arc<removal::Operation>> = None;
    let mut models_requested = false;
    let mut published = service.snapshot();
    loop {
        if service.stop.load(Ordering::Acquire)
            || registry.upgrade().is_none_or(|r| !r.current(service))
        {
            break;
        }
        if removal::attach_preparation(service, &mut connection, &scope).is_err() {
            break;
        }
        removal::retire_active(service, &mut active);
        if service.login.lock().is_ok_and(|login| {
            login
                .as_ref()
                .is_some_and(|pending| pending.cancel.load(Ordering::Acquire))
        }) && matches!(
            connection.state(),
            AccountState::StartingLogin | AccountState::AwaitingBrowser
        ) {
            if removal::cancel_login(service, &mut connection, &scope).is_err() {
                break;
            }
        }
        if let Some(current) = active.as_mut() {
            if current.delivery.is_none()
                && current.call.cancel.load(Ordering::Acquire)
                && !current.cancel_sent
            {
                current.cancel_sent = true;
                if removal::cancel_turn(service, &mut connection, &scope).is_err() {
                    break;
                }
            }
        }
        match receiver.try_recv() {
            Ok(mut command) => {
                // Receiving transfers local ownership even if the profile was
                // revoked while queued. Physical teardown precedes release.
                let occupied = if let Command::Reply(call) = &mut command {
                    !adopt_reply_turn(service, call)
                } else {
                    false
                };
                let Some(registry) = registry.upgrade().filter(|r| r.current(service)) else {
                    retired_command = Some(command);
                    break;
                };
                let ordinary_retired = service.removal.lock().is_ok_and(|value| value.is_some());
                let result = match command {
                    Command::RemovalLocal(_) => Err(Fault::Busy),
                    Command::PrepareBrowser(call) => {
                        browser_reservation::prepare(service, &mut connection, &scope, *call);
                        Ok(())
                    }
                    Command::Removal(operation) => {
                        if operation.profile != service.profile || removal_active.is_some() {
                            Err(Fault::ScopeChanged)
                        } else {
                            let result =
                                removal::attach_preparation(service, &mut connection, &scope);
                            if result.is_ok() {
                                removal_active = Some(operation);
                            }
                            result
                        }
                    }
                    Command::Select(call) if ordinary_retired => {
                        let _ = call.result.send(Err(CodexFaultV1::ScopeChanged));
                        Ok(())
                    }
                    Command::Models | Command::Quota | Command::Logout if ordinary_retired => {
                        Ok(())
                    }
                    Command::Login(attempt) => {
                        let cancelled = ordinary_retired || attempt.cancel.load(Ordering::Acquire);
                        let result = if cancelled {
                            Ok(())
                        } else {
                            connection.login(&scope, &attempt.cancel)
                        };
                        if cancelled || result.is_err() {
                            // The actor either never issued this login or has
                            // already closed on a transport failure. Remove only
                            // this ceremony, never a replacement attempt.
                            if let Ok(mut view) = service.view.lock() {
                                if let Ok(mut pending) = service.login.lock() {
                                    if pending.as_ref().is_some_and(|value| value.id == attempt.id)
                                    {
                                        if let Some(value) = pending.take() {
                                            value.cancel.store(true, Ordering::Release);
                                        }
                                        view.login_attempt = None;
                                    }
                                }
                            }
                            state(service, connection.state());
                        }
                        result
                    }
                    Command::CancelLogin(attempt) => {
                        let matches = service.login.lock().is_ok_and(|state| {
                            state.as_ref().is_some_and(|pending| pending.id == attempt)
                        });
                        if matches
                            && matches!(
                                connection.state(),
                                AccountState::StartingLogin
                                    | AccountState::AwaitingBrowser
                                    | AccountState::Cancelling
                            )
                        {
                            removal::cancel_login(service, &mut connection, &scope)
                        } else {
                            removal::read_account(service, &mut connection, &scope)
                        }
                    }
                    Command::Logout => {
                        if let Ok(mut view) = service.view.lock() {
                            view.login_attempt = None;
                            view.selection = None;
                        }
                        connection.logout(&scope, &service.stop)
                    }
                    Command::Models => connection.refresh_catalog(&scope, &service.stop),
                    Command::Quota => {
                        let result = connection.request_quota_refresh(&scope, &service.stop);
                        if let Ok(observation) = connection.quota(&scope) {
                            quota(service, observation);
                        }
                        result
                    }
                    Command::Select(call) => {
                        let valid = match &call.selection {
                            page_choice::Selection::Model(_) => true, // prepared by native app catalog
                            page_choice::Selection::Codex(choice) => {
                                connection.catalog().is_some_and(|catalog| {
                                    catalog
                                        .select(&choice.model, selected_effort(choice.effort))
                                        .is_ok()
                                })
                            }
                        };
                        let response = if Instant::now() >= call.deadline {
                            Err(CodexFaultV1::OutcomeUnknown)
                        } else if !valid
                            || !call.expected.valid()
                            || call.expected.profile != service.profile
                        {
                            Err(CodexFaultV1::Invalid)
                        } else if let Some(store) = APP_STORE.get() {
                            let wanted = call.expected.updated(call.selection.clone());
                            let receipt = store.page_choice_call(
                                service.profile,
                                page_choice::Call::CompareAndSwap {
                                    expected: Box::new(call.expected),
                                    selection: call.selection,
                                },
                            );
                            match &receipt {
                                page_choice::Reply::Committed(value)
                                    if Some(value) == wanted.as_ref() =>
                                {
                                    if observe_selection(service, value, None) {
                                        Ok(receipt)
                                    } else {
                                        Err(CodexFaultV1::OutcomeUnknown)
                                    }
                                }
                                page_choice::Reply::Conflict(value)
                                    if value.profile == service.profile && value.valid() =>
                                {
                                    let _ = observe_selection(service, value, None);
                                    Ok(receipt)
                                }
                                _ => {
                                    selection_uncertain(service);
                                    Err(CodexFaultV1::OutcomeUnknown)
                                }
                            }
                        } else {
                            Err(CodexFaultV1::ProfileStorage)
                        };
                        if let Err(error) = response.as_ref() {
                            if let Ok(mut view) = service.view.lock() {
                                view.error = Some(*error);
                            }
                        } else if let Ok(mut view) = service.view.lock() {
                            view.error = None;
                        }
                        let _ = call.result.send(response);
                        Ok(())
                    }
                    Command::Reply(mut call) => {
                        if let Some(start) = call.browser.take() {
                            browser_worker::start(
                                service,
                                &mut connection,
                                &scope,
                                call,
                                *start,
                                &mut active,
                                &mut rejected_browser,
                            )
                        } else {
                            // Once Page has committed, only the actor may classify
                            // cancelled/changed input. Never free this turn merely
                            // because its waiting UI future was dropped.
                            if occupied || active.is_some() {
                                let _ = call.result.send(ReplyDelivery::Failed(
                                    PageAssistNativeFaultV1::Unavailable,
                                ));
                                service.request_stop();
                                Err(Fault::Busy)
                            } else {
                                let key =
                                    ConversationKey::from_native(call.request.conversation.clone());
                                match key {
                                    None => {
                                        let _ = call.result.send(ReplyDelivery::Failed(
                                            PageAssistNativeFaultV1::Protocol,
                                        ));
                                        service.request_stop();
                                        Err(Fault::Protocol)
                                    }
                                    Some(key) => {
                                        // Captured natively before Page admits its first message. Never
                                        // inferred from renderer data or a restarted reply counter.
                                        let new_conversation = call.request.new_conversation;
                                        let result = connection.start_text(
                                            &scope,
                                            TextRequest {
                                                reply: u64::from(call.request.reply),
                                                previous_reply: call
                                                    .request
                                                    .previous_reply
                                                    .map(u64::from),
                                                admission: TextAdmission::from_native(
                                                    call.cancel.clone(),
                                                    service.account_revision.clone(),
                                                    call.account_revision,
                                                ),
                                                key,
                                                new_conversation,
                                                model: call.selection.model.clone(),
                                                effort: selected_effort(call.selection.effort),
                                                text: call.request.text.clone(),
                                            },
                                            &service.stop,
                                        );
                                        match result {
                                            Ok(()) => {
                                                active = Some(Active {
                                                    browser: false,
                                                    delivery: None,
                                                    call,
                                                    items: HashMap::new(),
                                                    cancel_sent: false,
                                                });
                                                Ok(())
                                            }
                                            Err(error) => {
                                                let _ = call
                                                    .result
                                                    .send(ReplyDelivery::Failed(page_fault(error)));
                                                // A failed actor admission is not a generic
                                                // local clean token. Retain the turn until
                                                // this original connection physically closes.
                                                service.request_stop();
                                                Err(error)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                };
                if let Err(error) = result {
                    state(service, connection.state());
                    if let Ok(mut view) = service.view.lock() {
                        view.error = Some(fault(error));
                    }
                    if connection.is_closed() {
                        break;
                    }
                }
                drop(registry);
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Some(operation) = removal_active.as_ref() {
            if active.is_none() && service.settling.lock().is_ok_and(|value| value.is_none()) {
                match removal::begin(operation, &mut connection, &scope) {
                    Ok(true) if connection.removal_ready(&scope) => break,
                    Ok(_) => (),
                    Err(error) => {
                        set_fault(service, fault(error));
                        break;
                    }
                }
            }
        }
        let (events, poll_fault) = match connection.poll(&scope, &service.stop) {
            Ok(events) => (events, None),
            Err(error) => (
                connection.take_failure_events(&scope).unwrap_or_default(),
                Some(error),
            ),
        };
        for event in events {
            if event.scope() != &scope {
                service.request_stop();
                continue;
            }
            if registry.upgrade().is_none_or(|r| !r.current(service)) {
                // Historical native receipts are not permission for another
                // tool/request. Same-pump retirement must not erase a genuine
                // Browser terminal; Page still checks exact display lifetime.
                if active.as_ref().is_some_and(|value| value.browser)
                    || service.queue.browser_occupied()
                {
                    browser_worker::historical(service, &scope, &mut active, vec![event]);
                }
                service.request_stop();
                continue;
            }
            let key = event.conversation().map(|key| key.as_str().to_owned());
            match event.into_event() {
                Event::BrowserSettled { reply, local } => {
                    if let Some(key) = key.as_deref() {
                        browser_worker::settled(service, key, reply, local, &mut active);
                    }
                }
                Event::Account(account) => {
                    state(service, account);
                    if account != AccountState::ChatGpt {
                        models_requested = false;
                    }
                    if removal_active
                        .as_ref()
                        .is_none_or(|operation| !operation.begun())
                        && matches!(account, AccountState::Cancelled | AccountState::Unknown)
                    {
                        let _ = removal::read_account(service, &mut connection, &scope);
                    }
                    if matches!(account, AccountState::ChatGpt | AccountState::SignedOut) {
                        if let Ok(mut login) = service.login.lock() {
                            *login = None;
                        }
                    }
                }
                Event::Catalog(value) => catalog(service, &value),
                Event::Quota(observation) => quota(service, observation),
                Event::OpenBrowser(url)
                    if service.removal.lock().is_ok_and(|value| value.is_none()) =>
                {
                    open_browser(registry.clone(), Arc::downgrade(service), url)
                }
                Event::OpenBrowser(_) => {}
                Event::NeverStarted { reply } => {
                    let matches = active.as_ref().is_some_and(|active| {
                        key.as_deref() == Some(active.call.request.conversation.as_str())
                            && reply == u64::from(active.call.request.reply)
                    });
                    if matches {
                        deliver_outcome(
                            &mut active,
                            NativeConversationOutcome {
                                status: NativeConversationStatus::NeverStarted,
                                final_response: None,
                            },
                        );
                        // This exact broker event requires proof+Skip ACK and
                        // actual prep Closed, or no prep write at all. It is
                        // not minted from the UI cancellation bit.
                        let mut settling =
                            service.settling.lock().unwrap_or_else(|e| e.into_inner());
                        if settling.as_ref().is_some_and(|held| {
                            !held.has_local_obligation()
                                && Some(held.conversation.as_str()) == key.as_deref()
                                && u64::from(held.reply) == reply
                        }) {
                            settling.take();
                        }
                    }
                }
                Event::HistoryUncertain(result) => {
                    if key.as_deref()
                        == active
                            .as_ref()
                            .map(|active| active.call.request.conversation.as_str())
                    {
                        deliver_outcome(
                            &mut active,
                            NativeConversationOutcome {
                                status: NativeConversationStatus::HistoryUncertain,
                                final_response: result.final_response().map(str::to_owned),
                            },
                        );
                    }
                }
                Event::Conversation(event) => {
                    if matches!(event, conversation::Event::Closed) {
                        let mut settling =
                            service.settling.lock().unwrap_or_else(|e| e.into_inner());
                        if active.is_none()
                            && settling.as_ref().is_some_and(|held| {
                                !held.has_local_obligation()
                                    && Some(held.conversation.as_str()) == key.as_deref()
                            })
                        {
                            // The broker emits Closed only after both original
                            // unsubscribe ACK and the matching thread/closed.
                            settling.take();
                        }
                        continue;
                    }
                    let Some(current) = active.as_mut().filter(|active| {
                        key.as_deref() == Some(active.call.request.conversation.as_str())
                    }) else {
                        continue;
                    };
                    match event {
                        conversation::Event::Delta { item_id, text } => {
                            if current.call.cancel.load(Ordering::Acquire)
                                || current.call.account_revision
                                    != service.account_revision.load(Ordering::Acquire)
                            {
                                continue;
                            }
                            let value = current.items.entry(item_id).or_default();
                            if value.len().saturating_add(text.len()) > 24 * 1024 {
                                current.call.cancel.store(true, Ordering::Release);
                                continue;
                            }
                            value.push_str(&text);
                            if (current.call.events)(NativeConversationEvent::TextSnapshot {
                                text: value.clone(),
                            })
                            .is_err()
                            {
                                current.call.cancel.store(true, Ordering::Release);
                            }
                        }
                        conversation::Event::ItemCompleted(message) => {
                            if current.call.cancel.load(Ordering::Acquire)
                                || current.call.account_revision
                                    != service.account_revision.load(Ordering::Acquire)
                            {
                                continue;
                            }
                            if message.text.len() > 24 * 1024 {
                                current.call.cancel.store(true, Ordering::Release);
                                continue;
                            }
                            current.items.insert(message.id, message.text.clone());
                            if (current.call.events)(NativeConversationEvent::TextSnapshot {
                                text: message.text,
                            })
                            .is_err()
                            {
                                current.call.cancel.store(true, Ordering::Release);
                            }
                        }
                        conversation::Event::Terminal(result) => {
                            let outcome = NativeConversationOutcome {
                                status: match result.outcome {
                                    conversation::Outcome::Completed => {
                                        NativeConversationStatus::Completed
                                    }
                                    conversation::Outcome::Interrupted => {
                                        NativeConversationStatus::Interrupted
                                    }
                                    conversation::Outcome::Failed => {
                                        NativeConversationStatus::Failed
                                    }
                                    conversation::Outcome::Unknown => {
                                        NativeConversationStatus::Unknown
                                    }
                                },
                                final_response: result.final_response().map(str::to_owned),
                            };
                            deliver_outcome(&mut active, outcome);
                        }
                        conversation::Event::TransportLost { .. } => {}
                        _ => {}
                    }
                }
            }
        }
        if let Some(error) = poll_fault {
            deliver_failure(&mut active, page_fault(error));
            set_fault(service, fault(error));
            break;
        }
        if removal_active
            .as_ref()
            .is_none_or(|operation| !operation.begun())
            && matches!(
                connection.state(),
                AccountState::Unknown | AccountState::Cancelled
            )
        {
            match removal::read_account(service, &mut connection, &scope) {
                Ok(()) | Err(Fault::Busy) => {}
                Err(error) => {
                    set_fault(service, fault(error));
                    break;
                }
            }
        }
        if service.removal.lock().is_ok_and(|value| value.is_none())
            && connection.state() == AccountState::ChatGpt
            && !models_requested
            && connection.catalog().is_none()
        {
            match connection.refresh_catalog(&scope, &service.stop) {
                Ok(()) => models_requested = true,
                Err(Fault::Busy) => {}
                Err(error) => {
                    set_fault(service, fault(error));
                    break;
                }
            }
        }
        // This comes from the held original Session's sealed schema proof,
        // never from account Connected, model names, or a caller preference.
        if let Ok(mut view) = service.view.lock() {
            view.text_only = !connection.browser_capable(&scope)
                || service.stop.load(Ordering::Acquire)
                || service.account_revision.load(Ordering::Acquire) == 0;
        }
        service.queue.set_ready(
            service.removal.lock().is_ok_and(|value| value.is_none())
                && connection.can_admit_browser(&scope)
                && !service.stop.load(Ordering::Acquire)
                && service.selection_revision.load(Ordering::Acquire) != u64::MAX,
        );
        let next = service.snapshot();
        if next != published {
            published = next;
            notify_models(service, registry);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if connection.browser_settlement_pending()
        || service.queue.browser_occupied()
        || rejected_browser.is_some()
    {
        let Some(drained) = browser_worker::drain(
            service,
            registry,
            &receiver,
            connection,
            &scope,
            &mut active,
            browser_worker::Exit {
                rejected: rejected_browser,
                pending: retired_command,
            },
        ) else {
            notify_models(service, registry);
            return;
        };
        connection = drained;
    }
    if let Some(current) = active.take() {
        let _ = current.call.result.send(ReplyDelivery::Failed(
            PageAssistNativeFaultV1::OutcomeUnknown,
        ));
    }
    let removal_operation = service.removal.lock().ok().and_then(|value| value.clone());
    if let Some(operation) = removal_operation {
        removal::finish(registry, &operation, connection);
        service.retire_ordinary();
        set_fault(service, CodexFaultV1::Unavailable);
        admission::release_text_after_close(
            &mut service.settling.lock().unwrap_or_else(|e| e.into_inner()),
        );
        notify_models(service, registry);
        removal::local_worker(service, registry, receiver);
        return;
    }
    let cancelled_login = service.login.lock().is_ok_and(|pending| {
        pending
            .as_ref()
            .is_some_and(|attempt| attempt.cancel.load(Ordering::Acquire))
    });
    service.request_stop();
    // Consuming close waits for actual process/Job/writer settlement. No logout.
    if let Err(error) = connection.close() {
        set_fault(service, fault(error));
    } else if service.snapshot().error.is_none() {
        // A closed transport is not an indefinitely pending ceremony and is
        // never evidence of sign-out. Only an explicit Check may reopen it.
        set_fault(
            service,
            if cancelled_login {
                CodexFaultV1::Cancelled
            } else {
                CodexFaultV1::Unavailable
            },
        );
    }
    admission::release_text_after_close(
        &mut service.settling.lock().unwrap_or_else(|e| e.into_inner()),
    );
    notify_models(service, registry);
}
fn notify_models(service: &Service, registry: &std::sync::Weak<Registry>) {
    let Some(registry) = registry.upgrade() else {
        return;
    };
    if registry.scope(&service.profile.to_string()) != Some(service.epoch) {
        return;
    }
    emit_to_privileged(
        &registry.app,
        MAIN_LABEL,
        "zephium:work-models-changed",
        &crate::work_models::WorkModelsChanged { version: 1 },
    );
}
fn open_browser(
    registry: std::sync::Weak<Registry>,
    service: std::sync::Weak<Service>,
    url: LoginUrl,
) {
    let Some(owner) = service.upgrade() else {
        return;
    };
    let Some(attempt) = owner.snapshot().login_attempt else {
        return;
    };
    let Some(current) = registry.upgrade().filter(|r| r.current(&owner)) else {
        return;
    };
    let app = current.app.clone();
    let dispatch = app.clone();
    let _ = dispatch.run_on_main_thread(move || {
        let Some(owner) = service.upgrade() else {
            return;
        };
        let Some(current) = registry.upgrade() else {
            return;
        };
        if !current.current(&owner)
            || shutdown_started(&app)
            || !owner.login.lock().is_ok_and(|login| {
                login.as_ref().is_some_and(|pending| {
                    pending.id == attempt && !pending.cancel.load(Ordering::Acquire)
                })
            })
        {
            return;
        }
        use windows::{
            core::{w, HSTRING, PCWSTR},
            Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        };
        let target = HSTRING::from(url.as_str());
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &target,
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as usize <= 32 {
            if let Ok(mut view) = owner.view.lock() {
                view.error = Some(CodexFaultV1::Unavailable);
            }
        }
    });
}
pub(super) fn install(app: &tauri::AppHandle) {
    REGISTRY.get_or_init(|| {
        let registry = Arc::new(Registry {
            app: app.clone(),
            services: Mutex::new(HashMap::new()),
            foreground: RwLock::new(None),
            epoch: AtomicU64::new(1),
            closed: AtomicBool::new(STOPPED.load(Ordering::Acquire)),
            removals: removal::Inventory::default(),
        });
        removal::install(&registry);
        registry
    });
}
pub(super) fn profiles_changed(view: &zephium_ipc::ProfilesView) {
    let Some(registry) = REGISTRY.get() else {
        return;
    };
    let current = (!view.private)
        .then(|| view.current.clone())
        .flatten()
        .filter(|id| {
            view.profiles
                .iter()
                .any(|row| row.id == *id && row.kind != zephium_ipc::ProfileKindView::Incognito)
        });
    let Ok(services) = registry.services.lock() else {
        return;
    };
    let Ok(mut foreground) = registry.foreground.write() else {
        return;
    };
    if registry.closed.load(Ordering::Acquire) {
        return;
    }
    if *foreground != current {
        for service in services.values() {
            service.retire_for_scope();
        }
        registry.epoch.fetch_add(1, Ordering::AcqRel);
        *foreground = current;
    }
    for (profile, service) in services.iter() {
        if !view.profiles.iter().any(|row| {
            row.id == profile.to_string() && row.kind != zephium_ipc::ProfileKindView::Incognito
        }) {
            service.retire_for_scope();
        }
    }
}
pub(super) fn stop() {
    STOPPED.store(true, Ordering::Release);
    if let Some(registry) = REGISTRY.get() {
        let Ok(owners) = registry.services.lock() else {
            registry.closed.store(true, Ordering::Release);
            return;
        };
        for owner in owners.values() {
            owner.request_stop();
        }
        registry.removals.cancel_all();
        registry.closed.store(true, Ordering::Release);
        registry.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut foreground) = registry.foreground.write() {
            *foreground = None;
        }
    }
}
pub(super) async fn drained() -> bool {
    let Some(registry) = REGISTRY.get() else {
        return true;
    };
    if !registry.closed.load(Ordering::Acquire) {
        return false;
    }
    let owners = match registry.services.lock() {
        Ok(owners) => owners.values().cloned().collect::<Vec<_>>(),
        Err(_) => return false,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if owners.iter().all(|owner| owner.fully_settled()) {
            for owner in owners {
                if let Some(join) = owner
                    .join
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    if join.join().is_err() {
                        return false;
                    }
                }
            }
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn allowed(
    caller: &WebviewWindow,
    expected: &str,
    command: CodexCommand,
) -> Option<ProfileId> {
    let profile = ProfileId::parse(expected).filter(|id| id.to_string() == expected)?;
    let app = caller.app_handle();
    if !authorize(caller, CallerPolicy::Main, command.native()) || shutdown_started(app) {
        return None;
    }
    let receiver = app.state::<Handle>().profiles();
    let view = tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(4)))
        .await
        .ok()?
        .ok()??;
    if shutdown_started(app)
        || view.private
        || view.current.as_deref() != Some(expected)
        || !view
            .profiles
            .iter()
            .any(|row| row.id == expected && row.kind != zephium_ipc::ProfileKindView::Incognito)
    {
        return None;
    }
    Some(profile)
}
fn valid_attempt(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
pub(super) async fn commit_page_selection(
    profile: ProfileId,
    expected: page_choice::Snapshot,
    selection: page_choice::Selection,
) -> Result<page_choice::Reply, CodexFaultV1> {
    if expected.profile != profile || !expected.valid() || !selection.valid() {
        return Err(CodexFaultV1::Invalid);
    }
    let registry = REGISTRY.get().ok_or(CodexFaultV1::Unavailable)?;
    let epoch = registry
        .scope(&profile.to_string())
        .ok_or(CodexFaultV1::ScopeChanged)?;
    let route = admission::route_selection(
        &registry.services,
        profile,
        epoch,
        || registry.scope(&profile.to_string()),
        |owner| {
            if owner.fully_settled() {
                admission::OwnerState::Settled
            } else if owner.stop.load(Ordering::Acquire) || owner.done.load(Ordering::Acquire) {
                admission::OwnerState::Retiring
            } else {
                admission::OwnerState::Active { epoch: owner.epoch }
            }
        },
        || {
            if matches!(selection, page_choice::Selection::Codex(_)) {
                return Err(CodexFaultV1::Unavailable);
            }
            let store = APP_STORE.get().ok_or(CodexFaultV1::ProfileStorage)?;
            Ok(store.page_choice_call(
                profile,
                page_choice::Call::CompareAndSwap {
                    expected: Box::new(expected.clone()),
                    selection: selection.clone(),
                },
            ))
        },
    )
    .map_err(|error| match error {
        admission::Error::Busy => CodexFaultV1::Busy,
        admission::Error::Closed => CodexFaultV1::ScopeChanged,
    })?;
    let receipt = match route {
        admission::SelectionRoute::Owner(owner) => {
            if !registry.current(&owner) {
                return Err(CodexFaultV1::ScopeChanged);
            }
            let (result, receive) = tokio::sync::oneshot::channel();
            {
                let mut view = owner.view.lock().map_err(|_| CodexFaultV1::Unavailable)?;
                owner.enqueue(Command::Select(Box::new(SelectionCall {
                    expected,
                    selection,
                    deadline: Instant::now() + Duration::from_secs(8),
                    result,
                })))?;
                // Only after actual local queue admission. A Busy rejection cannot
                // retire a still-running choice or pretend a new one was stored.
                if !owner.advance_revision() {
                    owner.request_stop();
                    return Err(CodexFaultV1::OutcomeUnknown);
                }
                view.error = None;
            }
            tokio::time::timeout(Duration::from_secs(8), receive)
                .await
                .map_err(|_| CodexFaultV1::OutcomeUnknown)?
                .map_err(|_| CodexFaultV1::OutcomeUnknown)??
        }
        admission::SelectionRoute::Committed(receipt) => receipt?,
    };
    if registry.scope(&profile.to_string()) != Some(epoch) {
        return Err(CodexFaultV1::OutcomeUnknown);
    }
    Ok(receipt)
}

pub(super) async fn dispatch(
    caller: WebviewWindow,
    expected: String,
    command: CodexCommand,
    attempt: Option<String>,
    selection: Option<Option<CodexSelectionV1>>,
) -> CodexViewV1 {
    let Some(registry) = REGISTRY.get() else {
        return CodexViewV1::unavailable(expected);
    };
    let Some(epoch) = registry.scope(&expected) else {
        return CodexViewV1::unavailable(expected);
    };
    let Some(profile) = allowed(&caller, &expected, command).await else {
        return CodexViewV1::unavailable(expected);
    };
    if registry.scope(&expected) != Some(epoch) {
        return CodexViewV1::unavailable(expected);
    }
    let candidate = if command == CodexCommand::RefreshQuota {
        // Quota observation must never start/restart an owner or an auth ceremony.
        current_service(profile).ok_or(CodexFaultV1::Unavailable)
    } else {
        registry.service(profile, epoch, command == CodexCommand::CheckConnection)
    };
    let service = match candidate {
        Ok(service) => service,
        Err(error) => {
            let mut view = CodexViewV1::unavailable(expected);
            view.error = Some(error);
            return view;
        }
    };
    if command == CodexCommand::SelectModel {
        let Some(selection) = selection else {
            let mut view = service.snapshot();
            view.error = Some(CodexFaultV1::Invalid);
            return view;
        };
        let prior = match read_selection(profile) {
            Ok(value) => value,
            Err(error) => {
                let mut view = service.snapshot();
                view.error = Some(error);
                return view;
            }
        };
        let desired = match selection {
            Some(choice) => page_choice::Selection::Codex(core_choice(choice)),
            None => page_choice::Selection::Model(prior.model.clone()),
        };
        let wanted = prior.updated(desired.clone());
        let receipt = commit_page_selection(profile, prior, desired).await;
        if registry.scope(&profile.to_string()) != Some(epoch) {
            return CodexViewV1::unavailable(expected);
        }
        let error = match receipt {
            Ok(page_choice::Reply::Committed(value)) if Some(&value) == wanted.as_ref() => {
                (!observe_selection(&service, &value, None)).then_some(CodexFaultV1::OutcomeUnknown)
            }
            Ok(page_choice::Reply::Conflict(_)) => Some(CodexFaultV1::ScopeChanged),
            Ok(_) => Some(CodexFaultV1::OutcomeUnknown),
            Err(error) => Some(error),
        };
        let mut view = service.snapshot();
        view.error = error;
        return view;
    }
    if matches!(
        command,
        CodexCommand::Account | CodexCommand::CheckConnection
    ) {
        refresh_selection(&service);
    }
    // Hold the view publication lock through bounded queue admission. A worker
    // notification cannot interpose between retiring old clients and deciding
    // whether this exact intent was actually queued.
    let mut admission_view = None;
    let mut rollback_view = None;
    let operation = match command {
        CodexCommand::Account | CodexCommand::CheckConnection => None,
        CodexCommand::Connect => {
            if let Some(id) = attempt.filter(|s| valid_attempt(s)) {
                let cancel = Arc::new(AtomicBool::new(false));
                let mut view = match service.view.lock() {
                    Ok(view) => view,
                    Err(_) => return CodexViewV1::unavailable(expected),
                };
                if !matches!(
                    view.state,
                    CodexStateV1::SignedOut | CodexStateV1::Connected
                ) {
                    let mut refused = view.clone();
                    refused.error = Some(CodexFaultV1::Busy);
                    return refused;
                }
                let mut pending = match service.login.lock() {
                    Ok(value) => value,
                    Err(_) => return CodexViewV1::unavailable(expected),
                };
                if pending.is_some() {
                    let mut refused = view.clone();
                    refused.error = Some(CodexFaultV1::Busy);
                    return refused;
                }
                if !service.advance_revision() {
                    return CodexViewV1::unavailable(expected);
                }
                rollback_view = Some(view.clone());
                *pending = Some(LoginAttempt {
                    id: id.clone(),
                    cancel: cancel.clone(),
                });
                view.login_attempt = Some(id.clone());
                view.state = CodexStateV1::SigningIn;
                view.quota = Default::default();
                view.error = None;
                drop(pending);
                admission_view = Some(view);
                Some(Command::Login(LoginAttempt { id, cancel }))
            } else {
                None
            }
        }
        CodexCommand::CancelLogin => attempt.filter(|s| valid_attempt(s)).and_then(|id| {
            let pending = service.login.lock().ok()?;
            let matching = pending.as_ref().filter(|pending| pending.id == id)?;
            matching.cancel.store(true, Ordering::Release);
            Some(Command::CancelLogin(id))
        }),
        CodexCommand::Logout => {
            {
                let Ok(mut view) = service.view.lock() else {
                    return CodexViewV1::unavailable(expected);
                };
                if !service.advance_revision() {
                    return CodexViewV1::unavailable(expected);
                }
                rollback_view = Some(view.clone());
                view.state = CodexStateV1::Preparing;
                view.quota = Default::default();
                view.login_attempt = None;
                admission_view = Some(view);
            }
            if let Ok(login) = service.login.lock() {
                if let Some(login) = login.as_ref() {
                    login.cancel.store(true, Ordering::Release);
                }
            }
            Some(Command::Logout)
        }
        CodexCommand::RefreshModels => Some(Command::Models),
        CodexCommand::RefreshQuota => {
            if service.snapshot().state != CodexStateV1::Connected {
                return service.snapshot();
            }
            Some(Command::Quota)
        }
        CodexCommand::SelectModel => None, // handled by the confirmed transaction path above
    };
    if !matches!(
        command,
        CodexCommand::Account | CodexCommand::CheckConnection
    ) && operation.is_none()
    {
        let mut view = service.snapshot();
        view.error = Some(CodexFaultV1::Invalid);
        return view;
    }
    if let Some(operation) = operation {
        if let Err(view) = enqueue_admitted(&service, operation, admission_view, rollback_view) {
            return view;
        }
    } else {
        drop(admission_view);
    }
    if registry.scope(&expected) != Some(epoch) {
        return CodexViewV1::unavailable(expected);
    }
    service.snapshot()
}
/// Queue admission and its exact never-enqueued rollback run while the same
/// presentation lock is held; account observations cannot interpose. This is
/// local queue evidence only, never proof of an original process write result.
fn enqueue_admitted(
    service: &Service,
    operation: Command,
    mut view: Option<MutexGuard<'_, CodexViewV1>>,
    previous: Option<CodexViewV1>,
) -> Result<(), CodexViewV1> {
    let unsent_login = if let Command::Login(login) = &operation {
        Some(login.id.clone())
    } else {
        None
    };
    if let Err(error) = service.enqueue(operation) {
        if let Some(view) = view.as_mut() {
            if let Some(previous) = previous {
                **view = previous;
            }
            if let Some(id) = unsent_login {
                if let Ok(mut pending) = service.login.lock() {
                    if pending.as_ref().is_some_and(|attempt| attempt.id == id) {
                        if let Some(attempt) = pending.take() {
                            attempt.cancel.store(true, Ordering::Release);
                        }
                    }
                }
            }
            view.error = Some(error);
            return Err((*view).clone());
        }
        let mut view = service.snapshot();
        view.error = Some(error);
        return Err(view);
    }
    Ok(())
}

// Only a validated native Page profile with persisted Codex selection may
// reserve on demand. No renderer path/ID is accepted and no auth is initiated.
fn page_service(profile: ProfileId) -> Option<Arc<Service>> {
    if let Some(service) = current_service(profile) {
        return Some(service);
    }
    if !read_selection(profile)
        .is_ok_and(|snapshot| snapshot.backend == page_choice::Backend::Codex)
    {
        // An uncertain preference is unavailable, not permission to start an
        // original runtime on behalf of a supposedly retained native choice.
        return None;
    }
    let registry = REGISTRY.get()?;
    let epoch = registry.scope(&profile.to_string())?;
    registry.service(profile, epoch, false).ok()
}
fn current_service(profile: ProfileId) -> Option<Arc<Service>> {
    let registry = REGISTRY.get()?;
    let service = registry.services.lock().ok()?.get(&profile)?.clone();
    registry.current(&service).then_some(service)
}
fn choice_from_view(view: &CodexViewV1) -> Option<NativeConversationChoice> {
    if view.state != CodexStateV1::Connected || !view.page_selected {
        return None;
    }
    let selected = view.selection.as_ref()?;
    let row = view.models.iter().find(|row| {
        row.id == selected.model
            && match selected.effort {
                CodexEffortSelectionV1::Default => row.default_effort.is_some(),
                CodexEffortSelectionV1::Explicit(effort) => row.efforts.contains(&effort),
            }
    })?;
    Some(NativeConversationChoice {
        id: row.id.clone(),
        name: row.name.clone(),
        source: PageAssistModelSourceV1::Codex,
        capabilities: if view.text_only {
            zephium_ipc::page_assist::PageAssistCapabilitiesV1::TextOnly
        } else {
            zephium_ipc::page_assist::PageAssistCapabilitiesV1::Page
        },
        effort: match selected.effort {
            CodexEffortSelectionV1::Default => None,
            CodexEffortSelectionV1::Explicit(value) => Some(effort(value).as_str().into()),
        },
    })
}
pub(super) fn page_choice(profile: ProfileId) -> Option<NativeConversationChoice> {
    let service = page_service(profile)?;
    let snapshot = read_selection(profile).ok()?;
    if snapshot.backend != page_choice::Backend::Codex
        || service.selection_revision.load(Ordering::Acquire) != snapshot.revision
    {
        return None;
    }
    choice_from_view(&service.snapshot())
}
pub(super) fn reserve_page(
    profile: ProfileId,
    expected: NativeConversationChoice,
    draft: NativeConversationDraft,
) -> Result<Box<dyn NativeConversationReservation>, PageBackendError> {
    reserve_page_value(profile, expected, draft, admission::TurnMode::Text)
        .map(|value| Box::new(value) as Box<dyn NativeConversationReservation>)
}

pub(super) async fn reserve_page_browser(
    profile: ProfileId,
    expected: NativeConversationChoice,
    draft: NativeConversationDraft,
    manifest: NativeBrowserManifest,
) -> Result<Box<dyn NativeConversationReservation>, PageBackendError> {
    browser_reservation::reserve(profile, expected, draft, manifest).await
}

fn reserve_page_value(
    profile: ProfileId,
    expected: NativeConversationChoice,
    draft: NativeConversationDraft,
    mode: admission::TurnMode,
) -> Result<Reservation, PageBackendError> {
    let required = match mode {
        admission::TurnMode::Text => zephium_ipc::page_assist::PageAssistCapabilitiesV1::TextOnly,
        admission::TurnMode::Browser => zephium_ipc::page_assist::PageAssistCapabilitiesV1::Page,
    };
    if expected.capabilities != required {
        return Err(PageBackendError::NativeConversation(
            PageAssistNativeFaultV1::UnsupportedInput,
        ));
    }
    let registry = REGISTRY.get().ok_or(PageBackendError::NativeConversation(
        PageAssistNativeFaultV1::Unavailable,
    ))?;
    let service = page_service(profile).ok_or(PageBackendError::NativeConversation(
        PageAssistNativeFaultV1::Unavailable,
    ))?;
    if !registry.current_now(&service) {
        return Err(PageBackendError::NativeConversation(
            PageAssistNativeFaultV1::ScopeChanged,
        ));
    }
    let snapshot = read_selection(profile)
        .map_err(|_| PageBackendError::NativeConversation(PageAssistNativeFaultV1::Unavailable))?;
    if snapshot.backend != page_choice::Backend::Codex
        || service.selection_revision.load(Ordering::Acquire) != snapshot.revision
    {
        return Err(PageBackendError::NativeConversation(
            PageAssistNativeFaultV1::ScopeChanged,
        ));
    }
    let (selection, account_revision, reserved) = {
        let view = service
            .view
            .try_lock()
            .map_err(|_| PageBackendError::Busy)?;
        let login = service
            .login
            .try_lock()
            .map_err(|_| PageBackendError::Busy)?;
        let revision = service.account_revision.load(Ordering::Acquire);
        if revision == 0 || login.is_some() || choice_from_view(&view).as_ref() != Some(&expected) {
            return Err(PageBackendError::NativeConversation(
                PageAssistNativeFaultV1::ScopeChanged,
            ));
        }
        let reserved = match mode {
            admission::TurnMode::Text => service.queue.reserve(),
            admission::TurnMode::Browser => service.queue.reserve_browser(),
        }
        .map_err(|_| PageBackendError::Busy)?;
        (
            view.selection
                .clone()
                .ok_or(PageBackendError::NativeConversation(
                    PageAssistNativeFaultV1::ScopeChanged,
                ))?,
            revision,
            reserved,
        )
    };
    Ok(Reservation {
        service,
        account_revision,
        selection_revision: snapshot.revision,
        selection,
        choice: expected,
        draft,
        reserved: Some(reserved),
    })
}
struct Reservation {
    account_revision: u64,
    selection_revision: u64,
    service: Arc<Service>,
    selection: CodexSelectionV1,
    choice: NativeConversationChoice,
    draft: NativeConversationDraft,
    reserved: Option<admission::Reserved>,
}
impl Registry {
    /// Nonblocking observation while Page holds its transcript commit lock.
    fn current_now(&self, service: &Service) -> bool {
        let Ok(foreground) = self.foreground.try_read() else {
            return false;
        };
        !self.closed.load(Ordering::Acquire)
            && !service.stop.load(Ordering::Acquire)
            && !service.done.load(Ordering::Acquire)
            && self.epoch.load(Ordering::Acquire) == service.epoch
            && foreground.as_deref() == Some(service.profile.to_string().as_str())
    }
}
struct CancelReply(Option<Arc<AtomicBool>>);
impl Drop for CancelReply {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.store(true, Ordering::Release);
        }
    }
}
impl Reservation {
    fn validate_current(&self, current_scope: bool) -> Result<(), PageAssistNativeFaultV1> {
        let view = self
            .service
            .view
            .try_lock()
            .map_err(|_| PageAssistNativeFaultV1::ScopeChanged)?;
        let login = self
            .service
            .login
            .try_lock()
            .map_err(|_| PageAssistNativeFaultV1::ScopeChanged)?;
        if !current_scope
            || self.service.stop.load(Ordering::Acquire)
            || self.service.done.load(Ordering::Acquire)
            || self.selection_revision != self.service.selection_revision.load(Ordering::Acquire)
            || self.account_revision == 0
            || self.account_revision != self.service.account_revision.load(Ordering::Acquire)
            || login.is_some()
            || view.selection.as_ref() != Some(&self.selection)
            || choice_from_view(&view).as_ref() != Some(&self.choice)
            || self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !self.service.queue.validate(reserved))
        {
            return Err(PageAssistNativeFaultV1::ScopeChanged);
        }
        Ok(())
    }
}
impl NativeConversationReservation for Reservation {
    fn validate(&self) -> Result<(), PageAssistNativeFaultV1> {
        self.validate_current(
            REGISTRY
                .get()
                .is_some_and(|registry| registry.current_now(&self.service)),
        )
    }
    fn commit(
        mut self: Box<Self>,
        reply: u32,
        cancel: Arc<AtomicBool>,
        events: NativeConversationEvents,
    ) -> BoxFuture<'static, Result<NativeConversationOutcome, PageAssistNativeFaultV1>> {
        // This whole handoff is synchronous. The returned future only waits;
        // even dropping it unpolled signals cancellation to the exact owner.
        let Some(registry) = REGISTRY.get() else {
            return Box::pin(async { Err(PageAssistNativeFaultV1::OutcomeUnknown) });
        };
        let Some(reserved) = self.reserved.take() else {
            return Box::pin(async { Err(PageAssistNativeFaultV1::Protocol) });
        };
        let guard = CancelReply(Some(cancel.clone()));
        let (send, receive) = tokio::sync::oneshot::channel();
        let request = NativeConversationRequest {
            conversation: self.draft.conversation,
            reply,
            new_conversation: self.draft.new_conversation,
            previous_reply: self.draft.previous_reply,
            text: self.draft.text,
        };
        let sent = self.service.queue.handoff(reserved, |turn| {
            Command::Reply(ReplyCall {
                turn: Some(turn),
                account_revision: self.account_revision,
                request,
                selection: self.selection,
                events,
                cancel,
                result: ReplySink::Text(send),
                browser: None,
            })
        });
        Box::pin(async move {
            let mut guard = guard;
            sent.map_err(|_| PageAssistNativeFaultV1::OutcomeUnknown)?;
            let result = receive
                .await
                .map_err(|_| PageAssistNativeFaultV1::OutcomeUnknown)?;
            guard.0 = None;
            result.receive(
                registry.scope(&self.service.profile.to_string()) == Some(self.service.epoch),
                registry.current(&self.service)
                    && self.account_revision
                        == self.service.account_revision.load(Ordering::Acquire),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_native_conversation_is_predispatch_history_mismatch_not_unknown_model_outcome() {
        assert_eq!(
            page_fault(Fault::UnknownConversation),
            PageAssistNativeFaultV1::HistoryDiverged
        );
        assert_eq!(
            page_fault(Fault::HistoryPolicy),
            PageAssistNativeFaultV1::HistoryDiverged
        );
        assert_eq!(
            page_fault(Fault::HistoryUncertain),
            PageAssistNativeFaultV1::OutcomeUnknown
        );
    }

    fn owned_service() -> (Service, admission::Receiver<Command>) {
        let profile = ProfileId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap();
        let (queue, receiver) = admission::channel(1);
        let mut view = CodexViewV1::unavailable(profile.to_string());
        view.state = CodexStateV1::SignedOut;
        view.error = None;
        (
            Service {
                profile,
                epoch: 1,
                stop: AtomicBool::new(false),
                done: AtomicBool::new(false),
                view: Mutex::new(view),
                login: Mutex::new(None),
                account_revision: Arc::new(AtomicU64::new(1)),
                selection_revision: AtomicU64::new(0),
                queue,
                settling: Mutex::new(None),
                browser_quarantine: Mutex::new(Vec::new()),
                join: Mutex::new(None),
                removal: Mutex::new(None),
            },
            receiver,
        )
    }
    fn queued_browser_reply(
        service: &Service,
    ) -> tokio::sync::oneshot::Receiver<ReplyDelivery<NativeBrowserCompletion>> {
        service.queue.set_ready(true);
        let reserved = service.queue.reserve_browser().unwrap();
        let (send, receive) = tokio::sync::oneshot::channel();
        service
            .queue
            .handoff(reserved, |turn| {
                Command::Reply(ReplyCall {
                    turn: Some(turn),
                    account_revision: 1,
                    request: NativeConversationRequest {
                        conversation: "owned-page".into(),
                        reply: 7,
                        new_conversation: true,
                        previous_reply: None,
                        text: "exact words".into(),
                    },
                    selection: CodexSelectionV1 {
                        model: "native-model".into(),
                        effort: CodexEffortSelectionV1::Default,
                    },
                    events: Arc::new(|_| Ok(())),
                    cancel: Arc::new(AtomicBool::new(false)),
                    result: ReplySink::Browser(send),
                    // These ownership tests never construct a native preparation proof
                    // or claim a provider send. The actual Turn determines Browser mode.
                    browser: None,
                })
            })
            .unwrap();
        receive
    }

    #[test]
    fn worker_panic_before_queued_reply_adoption_cannot_release_browser_owner() {
        let (service, receiver) = owned_service();
        let service = Arc::new(service);
        let mut receive = queued_browser_reply(&service);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _owned_receiver = receiver;
            panic!("synthetic worker unwind before command adoption");
        }));
        worker_returned(&service, result.is_err());
        assert!(service.done.load(Ordering::Acquire));
        assert!(!service.fully_settled());
        assert!(service.queue.browser_handoff_lost());
        assert_eq!(
            service.snapshot().error,
            Some(CodexFaultV1::RecoveryRequired)
        );
        assert!(receive.try_recv().is_err());
        service.queue.set_ready(true);
        assert!(matches!(
            service.queue.reserve_browser(),
            Err(admission::Error::Busy)
        ));
        assert!(matches!(
            service.queue.retire_if_idle(|| Ok(())),
            Err(admission::Error::Busy)
        ));
        let owners = Mutex::new(HashMap::from([(service.profile, service.clone())]));
        let route = admission::route_selection(
            &owners,
            service.profile,
            service.epoch,
            || Some(service.epoch),
            |owner| {
                if owner.fully_settled() {
                    admission::OwnerState::Settled
                } else {
                    admission::OwnerState::Retiring
                }
            },
            || -> () { panic!("quarantined owner is not an absent-owner commit path") },
        );
        assert!(matches!(route, Err(admission::Error::Busy)));
    }

    #[test]
    fn retired_queue_adopts_same_browser_turn_and_waits_for_exact_dual_reply() {
        let (service, receiver) = owned_service();
        let mut receive = queued_browser_reply(&service);
        service.request_stop();
        let Command::Reply(mut call) = receiver.try_recv().unwrap() else {
            panic!("exact reply");
        };
        assert!(adopt_reply_turn(&service, &mut call));
        assert!(call.turn.is_none());
        let mut active = Some(Active {
            browser: true,
            delivery: None,
            call,
            items: HashMap::new(),
            cancel_sent: false,
        });
        deliver_outcome(
            &mut active,
            NativeConversationOutcome {
                status: NativeConversationStatus::Completed,
                final_response: Some("confirmed final".into()),
            },
        );
        assert!(matches!(
            receive.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        // A later source failure cannot erase the original known terminal.
        deliver_failure(&mut active, PageAssistNativeFaultV1::ScopeChanged);
        worker_returned(&service, false);
        assert!(!service.fully_settled());
        browser_worker::settled(
            &service,
            "other-page",
            7,
            zephium_codex_broker::connection::BrowserLocalOutcome::Observed,
            &mut active,
        );
        browser_worker::settled(
            &service,
            "owned-page",
            8,
            zephium_codex_broker::connection::BrowserLocalOutcome::Observed,
            &mut active,
        );
        assert!(matches!(
            receive.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        // Exercise the actual Desktop dual-event consumer. This synthetic call
        // is not evidence that an original process or browser effect settled.
        browser_worker::settled(
            &service,
            "owned-page",
            7,
            zephium_codex_broker::connection::BrowserLocalOutcome::Observed,
            &mut active,
        );
        let outcome = receive.try_recv().unwrap().receive(true, false).unwrap();
        assert_eq!(
            outcome.outcome.final_response.as_deref(),
            Some("confirmed final")
        );
        assert_eq!(outcome.outcome.status, NativeConversationStatus::Completed);
        assert_eq!(
            outcome.settlement,
            NativeBrowserSettlement::Joined {
                local: NativeBrowserLocalOutcome::Observed
            }
        );
        assert!(active.is_none());
        assert!(service.fully_settled());
    }

    #[test]
    fn browser_successful_future_never_precedes_exact_dual_settlement() {
        let (service, receiver) = owned_service();
        let mut receive = queued_browser_reply(&service);
        let Command::Reply(mut call) = receiver.try_recv().unwrap() else {
            panic!("exact reply");
        };
        assert!(adopt_reply_turn(&service, &mut call));
        let mut active = Some(Active {
            browser: true,
            delivery: None,
            call,
            items: HashMap::new(),
            cancel_sent: false,
        });
        deliver_outcome(
            &mut active,
            NativeConversationOutcome {
                status: NativeConversationStatus::Completed,
                final_response: Some("done".into()),
            },
        );
        // Original terminal, actual local return alone, or physical close alone
        // never call the dual-event consumer and therefore cannot resolve it.
        assert!(matches!(
            receive.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        admission::release_text_after_close(&mut service.settling.lock().unwrap());
        assert!(service.queue.browser_occupied());
        assert!(matches!(
            receive.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        browser_worker::settled(
            &service,
            "owned-page",
            7,
            zephium_codex_broker::connection::BrowserLocalOutcome::Observed,
            &mut active,
        );
        assert!(matches!(receive.try_recv(), Ok(ReplyDelivery::Settled(_))));
    }

    #[test]
    fn browser_known_terminal_quarantine_is_explicit_and_never_releases_slot() {
        let (service, receiver) = owned_service();
        let mut receive = queued_browser_reply(&service);
        let Command::Reply(mut call) = receiver.try_recv().unwrap() else {
            panic!("exact reply");
        };
        assert!(adopt_reply_turn(&service, &mut call));
        let mut active = Some(Active {
            browser: true,
            delivery: None,
            call,
            items: HashMap::new(),
            cancel_sent: false,
        });
        deliver_outcome(
            &mut active,
            NativeConversationOutcome {
                status: NativeConversationStatus::Completed,
                final_response: Some("known before receipt loss".into()),
            },
        );
        browser_worker::quarantine_waiter(&mut active);
        let result = receive.try_recv().unwrap().receive(true, false).unwrap();
        assert_eq!(result.settlement, NativeBrowserSettlement::Quarantined);
        assert_eq!(
            result.outcome.final_response.as_deref(),
            Some("known before receipt loss")
        );
        worker_returned(&service, false);
        assert!(!service.fully_settled());
        assert!(service.queue.browser_occupied());
        service.queue.set_ready(true);
        assert!(matches!(
            service.queue.reserve_browser(),
            Err(admission::Error::Busy)
        ));
    }

    #[test]
    fn browser_dual_receipt_keeps_individual_result_loss_distinct_from_provider_outcome() {
        let (service, receiver) = owned_service();
        let mut receive = queued_browser_reply(&service);
        let Command::Reply(mut call) = receiver.try_recv().unwrap() else {
            panic!("exact reply");
        };
        assert!(adopt_reply_turn(&service, &mut call));
        let mut active = Some(Active {
            browser: true,
            delivery: None,
            call,
            items: HashMap::new(),
            cancel_sent: false,
        });
        deliver_outcome(
            &mut active,
            NativeConversationOutcome {
                status: NativeConversationStatus::Completed,
                final_response: Some("original known".into()),
            },
        );
        browser_worker::settled(
            &service,
            "owned-page",
            7,
            zephium_codex_broker::connection::BrowserLocalOutcome::ResultUnobserved,
            &mut active,
        );
        let result = receive.try_recv().unwrap().receive(true, true).unwrap();
        assert_eq!(
            result.settlement,
            NativeBrowserSettlement::Joined {
                local: NativeBrowserLocalOutcome::ResultUnobserved
            }
        );
        assert_eq!(result.outcome.status, NativeConversationStatus::Completed);
        assert_eq!(
            result.outcome.final_response.as_deref(),
            Some("original known")
        );
        assert!(!service.queue.browser_occupied());
    }

    #[test]
    fn busy_lifecycle_gate_leaves_account_login_and_model_presentation_untouched() {
        let (service, _receiver) = owned_service();
        let login_cancel = Arc::new(AtomicBool::new(false));
        *service.login.lock().unwrap() = Some(LoginAttempt {
            id: "owned-login".into(),
            cancel: login_cancel.clone(),
        });
        service.queue.set_ready(true);
        let reserved = service.queue.reserve().unwrap();
        let before = service.snapshot();
        let revision = service.account_revision.load(Ordering::Acquire);
        assert_eq!(
            service.queue.retire_if_idle(|| {
                *service.removal.lock().unwrap() = Some(removal::test_operation(service.profile));
                service.account_revision.store(0, Ordering::Release);
                Ok(())
            }),
            Err(admission::Error::Busy)
        );
        assert!(service.removal.lock().unwrap().is_none());
        assert_eq!(service.account_revision.load(Ordering::Acquire), revision);
        assert_eq!(service.snapshot(), before);
        assert!(!login_cancel.load(Ordering::Acquire));
        assert!(service.queue.validate(&reserved));
    }
    #[test]
    fn foreground_retirement_preserves_only_the_distinct_removal_action() {
        let (service, _receiver) = owned_service();
        let action = removal::test_operation(service.profile);
        *service.removal.lock().unwrap() = Some(action.clone());
        service.retire_for_scope();
        assert_eq!(service.account_revision.load(Ordering::Acquire), 0);
        assert!(!service.stop.load(Ordering::Acquire));
        assert!(action.current());
        service.request_stop();
        assert!(!action.current());
        assert!(service.stop.load(Ordering::Acquire));
    }
    #[test]
    fn removal_uses_existing_queue_capacity_and_retains_one_unsent_operation() {
        let (service, receiver) = owned_service();
        service.enqueue(Command::Models).unwrap();
        let action = removal::test_operation(service.profile);
        *service.removal.lock().unwrap() = Some(action.clone());
        service.retire_ordinary();
        assert_eq!(
            service.enqueue(Command::Removal(action.clone())),
            Err(CodexFaultV1::Busy)
        );
        assert!(matches!(receiver.try_recv(), Ok(Command::Models)));
        service.enqueue(Command::Removal(action.clone())).unwrap();
        match receiver.try_recv() {
            Ok(Command::Removal(actual)) => assert!(Arc::ptr_eq(&actual, &action)),
            _ => panic!("same removal envelope required"),
        }
        assert!(action.current());
        assert!(!action.begun());
        assert_eq!(
            service.enqueue(Command::Models),
            Err(CodexFaultV1::ScopeChanged)
        );
    }
    #[test]
    fn terminal_then_owner_fault_keeps_authoritative_final_text() {
        let (service, _receiver) = owned_service();
        let receipt = ReplyDelivery::Settled(NativeConversationOutcome {
            status: NativeConversationStatus::Completed,
            final_response: Some("original final".into()),
        });
        service.request_stop();
        let result = receipt
            .receive(true, service.account_revision.load(Ordering::Acquire) == 1)
            .unwrap();
        assert!(matches!(result.status, NativeConversationStatus::Completed));
        assert_eq!(result.final_response.as_deref(), Some("original final"));
    }
    #[test]
    fn history_uncertain_then_close_keeps_receipt_but_never_crosses_profile_epoch() {
        let (service, _receiver) = owned_service();
        service.request_stop();
        let make = || {
            ReplyDelivery::Settled(NativeConversationOutcome {
                status: NativeConversationStatus::HistoryUncertain,
                final_response: Some("confirmed text; unsaved history".into()),
            })
        };
        let result = make().receive(true, false).unwrap();
        assert!(matches!(
            result.status,
            NativeConversationStatus::HistoryUncertain
        ));
        assert!(result.final_response.is_some());
        assert!(matches!(
            make().receive(false, false),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        ));
        assert!(matches!(
            ReplyDelivery::Failed(PageAssistNativeFaultV1::Protocol).receive(true, false),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        ));
    }
    #[test]
    fn stopped_service_cannot_revive_its_writer_revision() {
        let (service, _receiver) = owned_service();
        let cancel = Arc::new(AtomicBool::new(false));
        *service.login.lock().unwrap() = Some(LoginAttempt {
            id: "first".into(),
            cancel: cancel.clone(),
        });
        assert!(service.advance_revision());
        service.request_stop();
        assert_eq!(service.account_revision.load(Ordering::Acquire), 0);
        assert!(cancel.load(Ordering::Acquire));
        assert!(!service.advance_revision());
        assert!(matches!(
            service.enqueue(Command::Models),
            Err(CodexFaultV1::ScopeChanged)
        ));
    }
    #[test]
    fn retired_or_failed_owner_never_projects_cached_current_quota() {
        use zephium_ipc::codex_quota::{CodexQuotaStateV1, CodexQuotaViewV1};
        for retirement in 0..4 {
            let (service, _receiver) = owned_service();
            {
                let mut view = service.view.lock().unwrap();
                view.text_only = false;
                view.quota = CodexQuotaViewV1 {
                    state: CodexQuotaStateV1::Current,
                    account_epoch: Some("1".into()),
                    observed_at_ms: Some("123".into()),
                    checking: true,
                    ..Default::default()
                };
                if retirement == 0 {
                    // This is an actual call pattern: stop under a held view
                    // lock must remain nonblocking and never re-lock the view.
                    service.request_stop();
                }
            }
            match retirement {
                0 => {}
                1 => service.done.store(true, Ordering::Release),
                2 => service.account_revision.store(0, Ordering::Release),
                3 => set_fault(&service, CodexFaultV1::Unavailable),
                _ => unreachable!(),
            }
            assert_eq!(service.snapshot().quota, CodexQuotaViewV1::default());
            assert!(service.snapshot().text_only);
            if retirement == 3 {
                assert_eq!(
                    service.view.lock().unwrap().quota,
                    CodexQuotaViewV1::default()
                );
            }
        }
    }
    #[test]
    fn full_queue_restores_only_the_never_enqueued_login_presentation() {
        let (service, receiver) = owned_service();
        service.enqueue(Command::Models).unwrap();
        let mut view = service.view.lock().unwrap();
        let previous = view.clone();
        assert!(service.advance_revision());
        let cancel = Arc::new(AtomicBool::new(false));
        *service.login.lock().unwrap() = Some(LoginAttempt {
            id: "first".into(),
            cancel: cancel.clone(),
        });
        view.state = CodexStateV1::SigningIn;
        view.login_attempt = Some("first".into());
        let result = enqueue_admitted(
            &service,
            Command::Login(LoginAttempt {
                id: "first".into(),
                cancel: cancel.clone(),
            }),
            Some(view),
            Some(previous),
        )
        .unwrap_err();
        assert_eq!(result.state, CodexStateV1::SignedOut);
        assert_eq!(result.error, Some(CodexFaultV1::Busy));
        assert!(result.login_attempt.is_none());
        assert!(service.login.lock().unwrap().is_none());
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(service.account_revision.load(Ordering::Acquire), 2);
        assert!(matches!(receiver.try_recv(), Ok(Command::Models)));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
    #[test]
    fn successful_queue_admission_keeps_exact_login_and_nonadmissible_view() {
        let (service, receiver) = owned_service();
        let mut view = service.view.lock().unwrap();
        let previous = view.clone();
        assert!(service.advance_revision());
        let cancel = Arc::new(AtomicBool::new(false));
        *service.login.lock().unwrap() = Some(LoginAttempt {
            id: "first".into(),
            cancel: cancel.clone(),
        });
        view.state = CodexStateV1::SigningIn;
        view.login_attempt = Some("first".into());
        enqueue_admitted(
            &service,
            Command::Login(LoginAttempt {
                id: "first".into(),
                cancel: cancel.clone(),
            }),
            Some(view),
            Some(previous),
        )
        .unwrap();
        assert_eq!(service.snapshot().state, CodexStateV1::SigningIn);
        assert!(choice_from_view(&service.snapshot()).is_none());
        assert!(!cancel.load(Ordering::Acquire));
        assert!(
            matches!(receiver.try_recv(), Ok(Command::Login(LoginAttempt { id, .. })) if id == "first")
        );
    }
    #[test]
    fn displayed_choice_requires_same_native_catalog_selection_and_backend_intent() {
        let (service, _receiver) = owned_service();
        let mut view = service.snapshot();
        view.state = CodexStateV1::Connected;
        view.page_selected = true;
        view.models.push(CodexModelV1 {
            id: "native-id".into(),
            name: "Original Model".into(),
            efforts: vec![CodexEffortV1::High],
            default_effort: Some(CodexEffortV1::High),
        });
        view.selection = Some(CodexSelectionV1 {
            model: "native-id".into(),
            effort: CodexEffortSelectionV1::Explicit(CodexEffortV1::High),
        });
        let choice = choice_from_view(&view).unwrap();
        assert_eq!(choice.id, "native-id");
        assert_eq!(choice.name, "Original Model");
        assert_eq!(choice.effort.as_deref(), Some("high"));
        assert_eq!(
            choice.capabilities,
            zephium_ipc::page_assist::PageAssistCapabilitiesV1::TextOnly
        );
        // Projection only: the production writer of this observation is the
        // same held Connection::browser_capable schema proof above.
        view.text_only = false;
        let browser = choice_from_view(&view).unwrap();
        assert_eq!(
            browser.capabilities,
            zephium_ipc::page_assist::PageAssistCapabilitiesV1::Page
        );
        assert_eq!(browser.id, choice.id);
        assert_eq!(browser.name, choice.name);
        assert_ne!(browser, choice);
        view.selection.as_mut().unwrap().effort =
            CodexEffortSelectionV1::Explicit(CodexEffortV1::Max);
        assert!(choice_from_view(&view).is_none());
        view.selection.as_mut().unwrap().effort =
            CodexEffortSelectionV1::Explicit(CodexEffortV1::High);
        view.page_selected = false;
        assert!(choice_from_view(&view).is_none());
    }
    fn test_reservation() -> (Reservation, admission::Receiver<Command>) {
        let (service, receiver) = owned_service();
        {
            let mut view = service.view.lock().unwrap();
            view.state = CodexStateV1::Connected;
            view.page_selected = true;
            view.models.push(CodexModelV1 {
                id: "same-model".into(),
                name: "Original Model".into(),
                efforts: vec![CodexEffortV1::High],
                default_effort: Some(CodexEffortV1::High),
            });
            view.selection = Some(CodexSelectionV1 {
                model: "same-model".into(),
                effort: CodexEffortSelectionV1::Explicit(CodexEffortV1::High),
            });
        }
        service.queue.set_ready(true);
        let choice = choice_from_view(&service.snapshot()).unwrap();
        let selection = service.snapshot().selection.unwrap();
        let reserved = service.queue.reserve().unwrap();
        (
            Reservation {
                account_revision: 1,
                selection_revision: 0,
                service: Arc::new(service),
                choice,
                selection,
                draft: NativeConversationDraft {
                    conversation: "native-page".into(),
                    new_conversation: true,
                    previous_reply: None,
                    text: "original words".into(),
                },
                reserved: Some(reserved),
            },
            receiver,
        )
    }
    #[test]
    fn settled_owner_projects_confirmed_configuration_without_reopening_admission() {
        let (reservation, _receiver) = test_reservation();
        let service = reservation.service.clone();
        service.request_stop();
        service.done.store(true, Ordering::Release);
        service.view.lock().unwrap().state = CodexStateV1::Unavailable;
        let prior = page_choice::Snapshot {
            profile: service.profile,
            revision: 4,
            backend: page_choice::Backend::Codex,
            model: None,
            codex: Some(core_choice(reservation.selection.clone())),
        };
        service.selection_revision.store(4, Ordering::Release);
        let committed = prior.updated(page_choice::Selection::Model(None)).unwrap();
        assert!(observe_selection(&service, &committed, Some(0)));
        let view = service.snapshot();
        assert!(!view.page_selected);
        assert_eq!(view.selection, prior.codex.clone().map(view_choice));
        assert_eq!(view.state, CodexStateV1::Unavailable);
        assert_eq!(service.account_revision.load(Ordering::Acquire), 0);
        assert_eq!(service.selection_revision.load(Ordering::Acquire), 5);
        assert!(!observe_selection(&service, &prior, Some(0)));
        assert!(!service.snapshot().page_selected);
        assert!(reservation.validate_current(true).is_err());
        drop(reservation);
        assert!(matches!(
            service.queue.reserve(),
            Err(admission::Error::Busy)
        ));
    }
    #[test]
    fn reservation_rechecks_scope_and_same_model_account_revision_without_io() {
        let (reservation, _receiver) = test_reservation();
        assert!(reservation.validate_current(true).is_ok());
        assert_eq!(
            reservation.validate_current(false),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        );
        assert!(reservation.service.advance_revision());
        assert_eq!(
            reservation.validate_current(true),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        );
        assert!(matches!(
            reservation.service.queue.reserve(),
            Err(admission::Error::Busy)
        ));
    }
    #[test]
    fn reservation_validation_is_nonblocking_and_drop_before_handoff_releases() {
        let (reservation, _receiver) = test_reservation();
        let service = reservation.service.clone();
        let lock = service.view.lock().unwrap();
        assert_eq!(
            reservation.validate_current(true),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        );
        drop(lock);
        assert!(reservation.validate_current(true).is_ok());
        drop(reservation);
        assert!(service.queue.reserve().is_ok());
    }
    #[test]
    fn revoked_reservation_cannot_validate_after_stop_or_selection_replacement() {
        let (reservation, _receiver) = test_reservation();
        reservation.service.view.lock().unwrap().page_selected = false;
        assert_eq!(
            reservation.validate_current(true),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        );
        reservation.service.view.lock().unwrap().page_selected = true;
        reservation.service.request_stop();
        assert_eq!(
            reservation.validate_current(true),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        );
    }
}
