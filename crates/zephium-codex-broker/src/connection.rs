//! Serialized native owner of one original app-server connection. No generic
//! RPC endpoint, provider thread ID adoption, credential export or automatic retry.
use crate::{
    catalog::{Catalog, CatalogBuilder, EffortSelection, ModelChoice},
    compatibility::{self, ConfigProjection},
    conversation::{self, Command, Conversation, Phase, Scope},
    discovery,
    managed_account::{
        self, Effect, InitializedAccountReceipt, LoginAction, LogoutAction, ManagedAccount,
    },
    profile::{
        native::{random_id, ProfileLease},
        ProfileId,
    },
    protocol,
    text_policy::{self, PreparedText},
    windows::{self, Session, TurnHistory, TurnIntent, TurnOperation},
    wire, RuntimePaths,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use zephium_core::codex_store as durable;
pub use zephium_provider_accounts::codex::{AccountState, LoginUrl};
/// The trusted native adapter binds one Store actor and profile at construction.
/// Renderer data cannot select a database, profile, grant, or durability receipt.
pub trait Associations: Send {
    fn call(&mut self, call: durable::Call) -> durable::Reply;
}
const PREPARE: Duration = Duration::from_secs(120);
const RPC: Duration = Duration::from_secs(60);
const LOGIN: Duration = Duration::from_secs(5 * 60);
const TURN: Duration = Duration::from_secs(90);
const SESSION: Duration = Duration::from_secs(4 * 60 * 60);
const CLOSE: Duration = Duration::from_secs(30);
const MAX_CONVERSATIONS: usize = 64;
const MAX_POLL_FRAMES: usize = 32;
const MAX_NOTIFICATION_FRAMES: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    ScopeChanged,
    Busy,
    Closed,
    NotConnected,
    UnsupportedRuntime,
    RuntimeUnavailable,
    Profile,
    RecoveryRequired,
    Vault,
    Cancelled,
    Timeout,
    Transport,
    Protocol,
    Policy,
    UnsupportedAction,
    Upstream,
    Limit,
    UnsupportedText,
    UnknownConversation,
    HistoryUncertain,
    HistoryDiverged,
    PersistenceUnavailable,
}
/// A native Page conversation key, not a provider thread ID or resumed history.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ConversationKey(String);
impl ConversationKey {
    pub fn from_native(value: String) -> Option<Self> {
        durable::valid_conversation(&value).then_some(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Retained native cancellation and selection generation. A renderer cannot
/// construct this through IPC; the owning actor supplies its exact live atomics.
#[derive(Clone)]
pub struct TextAdmission {
    call_cancel: Arc<AtomicBool>,
    account_revision: Arc<AtomicU64>,
    expected_revision: u64,
}
impl TextAdmission {
    pub fn from_native(
        call_cancel: Arc<AtomicBool>,
        account_revision: Arc<AtomicU64>,
        expected_revision: u64,
    ) -> Self {
        Self {
            call_cancel,
            account_revision,
            expected_revision,
        }
    }
    fn writer_fence(&self) -> windows::WriteFence {
        windows::WriteFence::text(
            self.call_cancel.clone(),
            self.account_revision.clone(),
            self.expected_revision,
        )
    }
    fn preparation_fence(&self) -> windows::WriteFence {
        windows::WriteFence::preparation(self.account_revision.clone(), self.expected_revision)
    }
    fn cancelled(&self) -> bool {
        self.call_cancel.load(Ordering::Acquire)
    }
    fn check_scope(&self) -> Result<(), Fault> {
        if self.account_revision.load(Ordering::Acquire) != self.expected_revision {
            return Err(Fault::ScopeChanged);
        }
        Ok(())
    }
}
/// Native actor-provided text operation; no provider thread ID or config data.
pub struct TextRequest {
    pub admission: TextAdmission,
    pub reply: u64,
    pub previous_reply: Option<u64>,
    pub key: ConversationKey,
    pub new_conversation: bool,
    pub model: String,
    pub effort: EffortSelection,
    pub text: String,
}
pub enum Event {
    Account(AccountState),
    OpenBrowser(LoginUrl),
    Catalog(Catalog),
    Conversation(conversation::Event),
    /// Original terminal is known, but durable continuation acknowledgement is not.
    HistoryUncertain(conversation::TurnResult),
    /// No model-frame handoff, exact durable Skip, and all preparation closed.
    NeverStarted {
        reply: u64,
    },
}
pub struct ScopedEvent {
    scope: Scope,
    conversation: Option<ConversationKey>,
    event: Event,
}
impl ScopedEvent {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn conversation(&self) -> Option<&ConversationKey> {
        self.conversation.as_ref()
    }
    pub fn into_event(self) -> Event {
        self.event
    }
}
enum PendingKind {
    Account,
    Catalog(Option<String>),
    Conversation(Command),
}
struct Pending {
    kind: PendingKind,
    deadline: Instant,
}
struct Prompt {
    operation: TurnOperation,
    intent: Option<TurnIntent>,
    unsent: Option<windows::MatchedUnsent>,
    acknowledged_revision: Option<u64>,
    admission: TextAdmission,
    reply: u64,
    key: ConversationKey,
    choice: ModelChoice,
    text: Option<PreparedText>,
    deadline: Instant,
    open_deadline: Instant,
    retry_at: Option<Instant>,
}

struct StoreBinding {
    associations: Box<dyn Associations>,
    scope: durable::Scope,
}
pub struct Connection {
    release_version: Option<String>,
    projection: ConfigProjection,
    scope: Scope,
    associations: Box<dyn Associations>,
    association_scope: durable::Scope,
    records: BTreeMap<ConversationKey, durable::Association>,
    session: Option<Session>,
    runtime: Arc<RuntimePaths>,
    account: ManagedAccount,
    catalog: Option<Catalog>,
    catalog_builder: Option<CatalogBuilder>,
    conversations: BTreeMap<ConversationKey, Conversation>,
    active: Option<ConversationKey>,
    prompt: Option<Prompt>,
    pending: BTreeMap<u64, Pending>,
    next_id: u64,
    deadline: Instant,
    login_deadline: Option<Instant>,
    login_fence: Option<windows::WriteFence>,
    account_deadline: Option<Instant>,
    closing_deadline: Option<Instant>,
    failure_events: Vec<ScopedEvent>,
    ready_events: Vec<ScopedEvent>,
    advisory_join: crate::notification_scope::AdvisoryJoin,
    settled_turns: BTreeMap<ConversationKey, (String, String)>,
    notification_frames: usize,
}
/// A native-held authenticated image that passed the exact production semantic
/// startup policy in a fresh signed-out disposable home. No account home was open.
pub(crate) struct CompatibleImage(Arc<crate::PinnedImage>);
impl CompatibleImage {
    pub(crate) fn prepare(
        image: Arc<crate::PinnedImage>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Self, Fault> {
        check(deadline, cancel)?;
        if image.kind() != crate::pin::ImageKind::Cli {
            return Err(Fault::Policy);
        }
        let runtime = Arc::new(RuntimePaths::fresh_managed().map_err(|_| Fault::Profile)?);
        let result = probe_session(image.clone(), runtime.clone(), deadline, cancel);
        let cleanup = Arc::try_unwrap(runtime)
            .map_err(|_| Fault::Transport)
            .and_then(|runtime| runtime.close().map_err(|_| Fault::Profile));
        result?;
        cleanup?;
        check(deadline, cancel)?;
        Ok(Self(image))
    }
}
impl Connection {
    pub fn release_version(&self) -> Option<&str> {
        self.release_version.as_deref()
    }
    pub fn association_scope(&self) -> durable::Scope {
        self.association_scope
    }
    pub fn grant_epoch(&self) -> u64 {
        self.association_scope.grant_epoch
    }
    /// The native application must reserve this profile/epoch before calling.
    /// Blocking verification may outlive the logical budget but remains owned.
    pub fn open(
        profile: ProfileId,
        epoch: u64,
        associations: Box<dyn Associations>,
        cached_release: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Self, Fault> {
        let preparation = Instant::now() + PREPARE;
        check(preparation, cancel)?;
        let detected =
            discovery::discover(preparation, cached_release, cancel).map_err(discovery_fault)?;
        let _origin = detected.origin();
        let prepared = CompatibleImage::prepare(detected.into_image(), preparation, cancel)?;
        check(preparation, cancel)?;
        // No authenticated profile or OS credential target is touched until the
        // exact retained image passed the disposable production-policy probe.
        crate::vault_probe::verify().map_err(|_| Fault::Vault)?;
        let lease = ProfileLease::open(profile).map_err(|e| match e {
            crate::profile::native::Error::RecoveryRequired => Fault::RecoveryRequired,
            crate::profile::native::Error::Busy => Fault::Busy,
            _ => Fault::Profile,
        })?;
        check(preparation, cancel)?;
        Self::bootstrap(
            profile,
            epoch,
            lease,
            prepared,
            associations,
            preparation,
            cancel,
        )
    }
    // Shared by product discovery and the fresh-only credential-free qualifier.
    // Image authority cannot be substituted with an arbitrary path or digest.
    pub(crate) fn bootstrap(
        profile: ProfileId,
        epoch: u64,
        lease: ProfileLease,
        prepared: CompatibleImage,
        mut associations: Box<dyn Associations>,
        admission_deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Self, Fault> {
        if lease.record.profile != profile || prepared.0.kind() != crate::pin::ImageKind::Cli {
            return Err(Fault::Policy);
        }
        let profile_scope = format!("{}-{}", profile.leaf(), hex(&lease.record.incarnation));
        let scope = Scope::new(
            profile_scope,
            random_id().map_err(|_| Fault::Profile)?,
            epoch,
        )
        .map_err(|_| Fault::ScopeChanged)?;
        let association_scope = match associations.call(durable::Call::Open {
            incarnation: lease.record.incarnation,
        }) {
            durable::Reply::Scope(value)
                if value.valid() && value.incarnation == lease.record.incarnation =>
            {
                value
            }
            _ => return Err(Fault::PersistenceUnavailable),
        };
        let runtime = Arc::new(RuntimePaths::persistent(lease).map_err(|_| Fault::Profile)?);
        check(admission_deadline, cancel)?;
        let deadline = Instant::now() + RPC;
        let release_version = prepared.0.version().to_owned();
        let projection = prepared.0.projection();
        let session = Session::spawn(prepared.0, Arc::clone(&runtime)).map_err(transport)?;
        check(deadline, cancel)?;
        let mut connection = Self::bootstrap_session(
            scope,
            session,
            runtime,
            StoreBinding {
                associations,
                scope: association_scope,
            },
            projection,
            deadline,
            cancel,
        )?;
        connection.release_version = Some(release_version);
        Ok(connection)
    }
    fn bootstrap_session(
        scope: Scope,
        mut session: Session,
        runtime: Arc<RuntimePaths>,
        storage: StoreBinding,
        projection: ConfigProjection,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Self, Fault> {
        let account =
            policy_handshake(&mut session, &runtime, &scope, projection, deadline, cancel)?;
        let mut connection = Self {
            release_version: None,
            projection,
            scope: scope.clone(),
            associations: storage.associations,
            association_scope: storage.scope,
            records: BTreeMap::new(),
            session: Some(session),
            runtime,
            account,
            catalog: None,
            catalog_builder: None,
            conversations: BTreeMap::new(),
            active: None,
            prompt: None,
            pending: BTreeMap::new(),
            next_id: 5,
            deadline: Instant::now() + SESSION,
            login_deadline: None,
            login_fence: None,
            account_deadline: None,
            closing_deadline: None,
            failure_events: Vec::new(),
            ready_events: Vec::new(),
            advisory_join: crate::notification_scope::AdvisoryJoin::default(),
            settled_turns: BTreeMap::new(),
            notification_frames: 0,
        };
        connection.read_account(&scope, cancel)?;
        Ok(connection)
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn state(&self) -> AccountState {
        self.account
            .state(&self.scope)
            .unwrap_or(AccountState::Closed)
    }
    pub fn catalog(&self) -> Option<&Catalog> {
        self.catalog.as_ref()
    }
    pub fn is_closed(&self) -> bool {
        self.session.is_none()
    }
    /// Observational readiness only. The native Page reservation and final
    /// writer admission still bind the exact scope/account/turn independently.
    pub fn can_admit_text(&self, scope: &Scope) -> bool {
        self.current(scope).is_ok()
            && self.state() == AccountState::ChatGpt
            && self.catalog.is_some()
            && self.idle().is_ok()
    }
    fn current(&self, scope: &Scope) -> Result<(), Fault> {
        if *scope != self.scope {
            Err(Fault::ScopeChanged)
        } else if self.session.is_none() {
            Err(Fault::Closed)
        } else {
            Ok(())
        }
    }
    fn idle(&self) -> Result<(), Fault> {
        if self.active.is_some() || !self.pending.is_empty() || self.prompt.is_some() {
            Err(Fault::Busy)
        } else {
            Ok(())
        }
    }
    fn id(&mut self) -> Result<u64, Fault> {
        self.next_id = self.next_id.checked_add(1).ok_or(Fault::Limit)?;
        Ok(self.next_id)
    }
    fn send(
        &mut self,
        message: Value,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<(), Fault> {
        self.send_fenced(message, deadline, cancel, None)
    }
    fn send_fenced(
        &mut self,
        message: Value,
        deadline: Instant,
        cancel: &AtomicBool,
        fence: Option<windows::WriteFence>,
    ) -> Result<(), Fault> {
        check(deadline, cancel).or_else(|f| self.fail(f))?;
        let mut bytes = serde_json::to_vec(&message).map_err(|_| Fault::Protocol)?;
        let result = self
            .session
            .as_mut()
            .ok_or(Fault::Closed)?
            .send_frame_fenced(&bytes, deadline.min(self.deadline), cancel, fence)
            .map_err(transport);
        bytes.fill(0);
        result.or_else(|f| self.fail(f))
    }
    pub fn read_account(&mut self, scope: &Scope, cancel: &AtomicBool) -> Result<(), Fault> {
        self.current(scope)?;
        self.idle()?;
        self.account_deadline = Some(Instant::now() + RPC);
        let event = self.account.read(scope).map_err(managed_fault)?;
        self.account_effect(event, cancel, &mut Vec::new())
    }
    pub fn login(&mut self, scope: &Scope, cancel: &Arc<AtomicBool>) -> Result<(), Fault> {
        self.current(scope)?;
        self.idle()?;
        self.login_fence = Some(windows::WriteFence::login(Arc::clone(cancel)));
        let event = self
            .account
            .login(LoginAction::from_native_action(scope.clone()))
            .map_err(managed_fault)?;
        self.login_deadline = Some(Instant::now() + LOGIN);
        self.account_deadline = Some(Instant::now() + RPC);
        self.account_effect(event, cancel, &mut Vec::new())
    }
    pub fn cancel_login(&mut self, scope: &Scope, cancel: &AtomicBool) -> Result<(), Fault> {
        self.current(scope)?;
        let event = self.account.cancel_login(scope).map_err(managed_fault)?;
        self.account_effect(event, cancel, &mut Vec::new())
    }
    pub fn logout(&mut self, scope: &Scope, cancel: &AtomicBool) -> Result<(), Fault> {
        self.current(scope)?;
        self.idle()?;
        let event = self
            .account
            .logout(LogoutAction::from_native_action(scope.clone()))
            .map_err(managed_fault)?;
        self.account_effect(event, cancel, &mut Vec::new())
    }
    fn account_effect(
        &mut self,
        event: managed_account::AccountEvent,
        cancel: &AtomicBool,
        out: &mut Vec<ScopedEvent>,
    ) -> Result<(), Fault> {
        if event.scope() != &self.scope {
            return self.fail(Fault::ScopeChanged);
        }
        if event.invalidates_admission() {
            self.catalog = None;
            self.catalog_builder = None;
            if self.active.is_some() || self.prompt.is_some() {
                return self.fail(Fault::NotConnected);
            }
        }
        match event.into_effect() {
            Effect::Ignored => {}
            Effect::Changed(state) => {
                self.account_deadline = None;
                if !matches!(
                    state,
                    AccountState::StartingLogin
                        | AccountState::AwaitingBrowser
                        | AccountState::Cancelling
                ) {
                    self.login_deadline = None;
                }
                out.push(self.event(None, Event::Account(state)));
            }
            Effect::OpenBrowser(url) => {
                self.account_deadline = None;
                out.push(self.event(None, Event::OpenBrowser(url)));
            }
            Effect::CloseRequired => return self.fail(Fault::Cancelled),
            Effect::RequestQueued => {
                let id = self.id()?;
                let request = self
                    .account
                    .take_request(&self.scope, id)
                    .map_err(|_| Fault::Protocol)?;
                if request.scope() != &self.scope || request.wire_id() != id {
                    return self.fail(Fault::ScopeChanged);
                }
                let deadline = *self
                    .account_deadline
                    .get_or_insert_with(|| Instant::now() + RPC);
                self.pending.insert(
                    id,
                    Pending {
                        kind: PendingKind::Account,
                        deadline,
                    },
                );
                // AtomicBool is checked by send_frame before any bytes. If cancelled
                // after logical handoff, settle; never send login to obtain cancel ID.
                let message = request.into_json();
                if matches!(
                    message.get("method").and_then(Value::as_str),
                    Some("account/login/start" | "account/logout")
                ) {
                    check(deadline, cancel).or_else(|f| self.fail(f))?;
                    let expected = self.association_scope;
                    let updated = match self
                        .associations
                        .call(durable::Call::AdvanceGrant { expected })
                    {
                        durable::Reply::Scope(value)
                            if value.incarnation == expected.incarnation
                                && value.grant_epoch
                                    == expected
                                        .grant_epoch
                                        .checked_add(1)
                                        .ok_or(Fault::Limit)? =>
                        {
                            value
                        }
                        _ => return self.fail(Fault::PersistenceUnavailable),
                    };
                    self.association_scope = updated;
                    self.conversations.clear();
                    self.records.clear();
                    self.settled_turns.clear();
                    self.advisory_join = crate::notification_scope::AdvisoryJoin::default();
                }
                let fence = if message.get("method").and_then(Value::as_str)
                    == Some("account/login/start")
                {
                    Some(self.login_fence.clone().ok_or(Fault::Protocol)?)
                } else {
                    None
                };
                self.send_fenced(message, deadline, cancel, fence)?;
            }
        }
        Ok(())
    }
    pub fn refresh_catalog(&mut self, scope: &Scope, cancel: &AtomicBool) -> Result<(), Fault> {
        self.current(scope)?;
        self.idle()?;
        if self.state() != AccountState::ChatGpt {
            return Err(Fault::NotConnected);
        }
        self.catalog = None;
        self.catalog_builder = Some(CatalogBuilder::new());
        self.catalog_page(Instant::now() + RPC, cancel)
    }
    fn catalog_page(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<(), Fault> {
        let cursor = self
            .catalog_builder
            .as_ref()
            .ok_or(Fault::Protocol)?
            .next_cursor()
            .map(str::to_owned);
        let id = self.id()?;
        self.pending.insert(
            id,
            Pending {
                kind: PendingKind::Catalog(cursor.clone()),
                deadline,
            },
        );
        self.send(wire::model_list(id, cursor.as_deref()), deadline, cancel)
    }
    /// A new key is admitted only for a genuinely new empty Page conversation.
    /// Existing history resumes only from the same profile/grant Store association;
    /// an uncertain Pending dispatch never authorizes automatic replay.
    pub fn start_text(
        &mut self,
        scope: &Scope,
        request: TextRequest,
        cancel: &AtomicBool,
    ) -> Result<(), Fault> {
        let TextRequest {
            admission,
            reply,
            previous_reply,
            key,
            new_conversation,
            model,
            effort,
            text,
        } = request;
        // A queued per-call Stop still reaches current grant/history validation.
        // It never bypasses scope, account, catalog or continuity admission.
        admission.check_scope()?;
        check(Instant::now() + RPC, cancel)?;
        self.current(scope)?;
        self.idle()?;
        if self.state() != AccountState::ChatGpt {
            return Err(Fault::NotConnected);
        }
        let choice = self
            .catalog
            .as_ref()
            .ok_or(Fault::NotConnected)?
            .select(&model, effort)
            .map_err(|_| Fault::Policy)?;
        if reply == 0 || reply > i64::MAX as u64 {
            return Err(Fault::Policy);
        }
        let loaded = match self.associations.call(durable::Call::Load {
            scope: self.association_scope,
            conversation: key.0.clone(),
        }) {
            durable::Reply::Association(value) => value,
            durable::Reply::Failed(durable::Fault::StaleGrant) => return Err(Fault::ScopeChanged),
            _ => return self.fail(Fault::PersistenceUnavailable),
        };
        if let Some(record) = loaded {
            self.validate_record(&key, &record)?;
            let recorded_previous = match record.dispatch {
                durable::Dispatch::Ready => None,
                durable::Dispatch::Skipped { reply: prior }
                | durable::Dispatch::Settled { reply: prior, .. } => {
                    if reply <= prior {
                        return Err(Fault::HistoryUncertain);
                    }
                    Some(prior)
                }
                durable::Dispatch::Pending { .. } => return Err(Fault::HistoryUncertain),
            };
            if previous_reply != recorded_previous {
                return Err(Fault::HistoryDiverged);
            }
            if new_conversation {
                return Err(Fault::Policy);
            }
            if !self.conversations.contains_key(&key) {
                if self.conversations.len() >= MAX_CONVERSATIONS {
                    return Err(Fault::Limit);
                }
                let restored = match &record.thread_id {
                    Some(thread) => {
                        Conversation::from_cold_store(self.scope.clone(), thread.clone())
                            .map_err(conversation_fault)?
                    }
                    None => Conversation::new(self.scope.clone()),
                };
                self.conversations.insert(key.clone(), restored);
            }
            self.remember_settled(&key, &record);
            self.records.insert(key.clone(), record);
        } else {
            if previous_reply.is_some() {
                return Err(Fault::HistoryDiverged);
            }
            if !new_conversation {
                return Err(Fault::UnknownConversation);
            }
            if self.conversations.contains_key(&key) {
                return self.fail(Fault::PersistenceUnavailable);
            }
            if self.conversations.len() >= MAX_CONVERSATIONS {
                return Err(Fault::Limit);
            }
            self.conversations
                .insert(key.clone(), Conversation::new(self.scope.clone()));
        }
        let phase = self.conversations.get(&key).ok_or(Fault::Protocol)?.phase();
        if !matches!(phase, Phase::New | Phase::Closed) {
            return Err(Fault::Busy);
        }
        let acknowledged_revision = self.records.get(&key).map(|r| r.revision);
        let operation = TurnOperation::new(
            self.scope.clone(),
            TurnHistory {
                incarnation: self.association_scope.incarnation,
                grant_epoch: self.association_scope.grant_epoch,
                conversation: key.0.clone(),
                reply,
                previous_reply,
                expected_revision: acknowledged_revision,
                thread_id: self.records.get(&key).and_then(|r| r.thread_id.clone()),
            },
            choice.model(),
            choice.selection(),
            &text,
        )
        .ok_or(Fault::Policy)?;
        let prepared = text_policy::prepare_text(text).map_err(|_| Fault::UnsupportedText)?;
        let intent = self
            .session
            .as_mut()
            .ok_or(Fault::Closed)?
            .reserve_turn(operation.clone())
            .map_err(transport)?;
        let now = Instant::now();
        self.notification_frames = 0;
        self.prompt = Some(Prompt {
            operation,
            intent: Some(intent),
            unsent: None,
            acknowledged_revision,
            admission,
            reply,
            key: key.clone(),
            choice,
            text: Some(prepared),
            deadline: now + TURN,
            open_deadline: now + RPC,
            retry_at: None,
        });
        self.active = Some(key);
        self.open_prompt(cancel)
    }
    fn check_prompt_scope(&mut self, cancel: &AtomicBool) -> Result<(), Fault> {
        let prompt = self.prompt.as_ref().ok_or(Fault::Protocol)?;
        check(prompt.deadline, cancel)
            .and_then(|()| prompt.admission.check_scope())
            .or_else(|f| self.fail(f))
    }
    fn prompt_cancelled(&self) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|p| p.admission.cancelled())
    }
    fn open_prompt(&mut self, cancel: &AtomicBool) -> Result<(), Fault> {
        self.check_prompt_scope(cancel)?;
        if self.prompt_cancelled() {
            self.reject_local_intent()?;
            return self.finish_unsent();
        }
        let id = self.id()?;
        let prompt = self.prompt.as_ref().ok_or(Fault::Protocol)?;
        let conversation = self
            .conversations
            .get_mut(&prompt.key)
            .ok_or(Fault::Protocol)?;
        let command = if conversation.phase() == Phase::New {
            conversation.begin_start(&self.scope, id, prompt.choice.clone())
        } else {
            conversation.begin_resume(&self.scope, id, prompt.choice.clone())
        }
        .map_err(conversation_fault)?;
        self.send_command(command, prompt.open_deadline, cancel)
    }
    fn reject_local_intent(&mut self) -> Result<(), Fault> {
        let prompt = self.prompt.as_mut().ok_or(Fault::Protocol)?;
        if prompt.unsent.is_some() {
            return Ok(());
        }
        let intent = prompt.intent.take().ok_or(Fault::Protocol)?;
        let proof = self
            .session
            .as_ref()
            .ok_or(Fault::Closed)?
            .reject_intent(intent)
            .ok_or(Fault::Protocol)?;
        prompt.unsent = Some(proof.consume(&prompt.operation).ok_or(Fault::Protocol)?);
        prompt.text = None;
        Ok(())
    }
    fn finish_unsent(&mut self) -> Result<(), Fault> {
        let prompt = self.prompt.as_ref().ok_or(Fault::Protocol)?;
        let key = prompt.key.clone();
        let reply = prompt.reply;
        if !prompt
            .unsent
            .as_ref()
            .is_some_and(|proof| proof.is_for(&prompt.operation))
            || !matches!(
                self.conversations.get(&key).ok_or(Fault::Protocol)?.phase(),
                Phase::New | Phase::Closed
            )
            || self
                .pending
                .values()
                .any(|p| matches!(p.kind, PendingKind::Conversation(_)))
        {
            return self.fail(Fault::Protocol);
        }
        let expected_revision = prompt.acknowledged_revision;
        prompt.admission.check_scope().or_else(|f| self.fail(f))?;
        if self.records.get(&key).map(|r| r.revision) != expected_revision {
            return self.fail(Fault::PersistenceUnavailable);
        }
        let _proof = self
            .prompt
            .as_mut()
            .and_then(|p| p.unsent.take())
            .ok_or(Fault::Protocol)?;
        self.persist(
            &key,
            durable::Call::Skip {
                scope: self.association_scope,
                conversation: key.0.clone(),
                expected_revision,
                reply,
            },
        )?;
        self.prompt = None;
        self.active = None;
        self.closing_deadline = None;
        self.ready_events
            .push(self.event(Some(key), Event::NeverStarted { reply }));
        Ok(())
    }
    fn send_command(
        &mut self,
        command: Command,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<(), Fault> {
        let id = command_id(&command);
        let preparation = matches!(
            command,
            Command::StartThread { .. } | Command::ResumeThread { .. }
        );
        if preparation {
            self.check_prompt_scope(cancel)?;
        }
        if let Command::StartTurn { thread_id, .. } = &command {
            self.check_prompt_scope(cancel)?;
            if self.prompt_cancelled() {
                self.reject_local_intent()?;
                self.conversations
                    .get_mut(self.active.as_ref().ok_or(Fault::Protocol)?)
                    .ok_or(Fault::Protocol)?
                    .abandon_unstarted(&self.scope, id)
                    .map_err(conversation_fault)?;
                self.closing_deadline = Some(Instant::now() + RPC);
                return self.maybe_unsubscribe(cancel);
            }
            let prompt = self.prompt.as_mut().ok_or(Fault::Protocol)?;
            let message = wire::command(
                &command,
                path(self.runtime.cwd())?,
                &mut prompt.text,
                self.projection,
            )
            .map_err(|_| Fault::Policy)?;
            let intent = prompt.intent.take().ok_or(Fault::Protocol)?;
            let turn = match intent.into_write(id, thread_id, message) {
                Ok(turn) => turn,
                Err(intent) => {
                    prompt.intent = Some(intent);
                    return self.fail(Fault::Policy);
                }
            };
            let key = prompt.key.clone();
            let reply = prompt.reply;
            let expected_revision = prompt
                .acknowledged_revision
                .ok_or(Fault::PersistenceUnavailable)?;
            self.persist(
                &key,
                durable::Call::Begin {
                    scope: self.association_scope,
                    conversation: key.0.clone(),
                    expected_revision,
                    reply,
                },
            )?;
            self.check_prompt_scope(cancel)?;
            let fence = self
                .prompt
                .as_ref()
                .ok_or(Fault::Protocol)?
                .admission
                .writer_fence();
            self.pending.insert(
                id,
                Pending {
                    kind: PendingKind::Conversation(command),
                    deadline,
                },
            );
            match self.session.as_mut().ok_or(Fault::Closed)?.send_turn_frame(
                turn,
                deadline.min(self.deadline),
                cancel,
                fence,
            ) {
                Ok(()) => Ok(()),
                Err(failure) => {
                    let fault = transport(failure.fault);
                    let Some(proof) = failure.never_started else {
                        return self.fail(fault);
                    };
                    let prompt = self.prompt.as_mut().ok_or(Fault::Protocol)?;
                    if proof.wire_id() != Some(id) {
                        return self.fail(Fault::Protocol);
                    }
                    prompt.unsent = Some(proof.consume(&prompt.operation).ok_or(Fault::Protocol)?);
                    self.pending.remove(&id);
                    self.conversations
                        .get_mut(&key)
                        .ok_or(Fault::Protocol)?
                        .abandon_unstarted(&self.scope, id)
                        .map_err(conversation_fault)?;
                    // Scope retirement or transport teardown is not a cold-close
                    // receipt. Only a healthy original owner may drain this path.
                    self.check_prompt_scope(cancel)?;
                    self.closing_deadline = Some(Instant::now() + RPC);
                    self.maybe_unsubscribe(cancel)
                }
            }
        } else {
            let mut empty = None;
            let prepared = self
                .prompt
                .as_mut()
                .map(|p| &mut p.text)
                .unwrap_or(&mut empty);
            let message = wire::command(
                &command,
                path(self.runtime.cwd())?,
                prepared,
                self.projection,
            )
            .map_err(|_| Fault::Policy)?;
            self.pending.insert(
                id,
                Pending {
                    kind: PendingKind::Conversation(command),
                    deadline,
                },
            );
            let fence = if preparation {
                Some(
                    self.prompt
                        .as_ref()
                        .ok_or(Fault::Protocol)?
                        .admission
                        .preparation_fence(),
                )
            } else {
                None
            };
            self.send_fenced(message, deadline, cancel, fence)
        }
    }
    pub fn cancel_turn(&mut self, scope: &Scope, cancel: &AtomicBool) -> Result<(), Fault> {
        self.current(scope)?;
        let key = self.active.clone().ok_or(Fault::Busy)?;
        let Some(prompt) = self.prompt.as_mut() else {
            return Ok(());
        };
        prompt.admission.call_cancel.store(true, Ordering::Release);
        if prompt.unsent.is_some() {
            return Ok(());
        }
        if prompt.intent.is_some() {
            let phase = self.conversations.get(&key).ok_or(Fault::Protocol)?.phase();
            if matches!(phase, Phase::New | Phase::Closed) {
                self.reject_local_intent()?;
                return self.finish_unsent();
            }
            if matches!(phase, Phase::Opening | Phase::Ready) {
                return Ok(());
            }
        }
        let events = self
            .conversations
            .get_mut(&key)
            .ok_or(Fault::Protocol)?
            .cancel(scope)
            .map_err(conversation_fault)?;
        self.conversation_effects(key, events, cancel, &mut Vec::new())
    }
    fn conversation_effects(
        &mut self,
        key: ConversationKey,
        events: Vec<conversation::Event>,
        cancel: &AtomicBool,
        out: &mut Vec<ScopedEvent>,
    ) -> Result<(), Fault> {
        // The reducer can drain a terminal buffered before the turn/start reply
        // into the same batch as TurnStarted. A failed earlier Store ack must
        // not erase that already-authentic terminal and label the model unknown.
        let known: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                conversation::Event::Terminal(result)
                    if result.outcome != conversation::Outcome::Unknown =>
                {
                    Some(result.clone())
                }
                _ => None,
            })
            .collect();
        let begin = out.len();
        let result = self.apply_conversation_effects(key.clone(), events, cancel, out);
        if result.is_err() {
            for terminal in known {
                let delivered = out[begin..].iter().any(|event| match &event.event {
                    Event::Conversation(conversation::Event::Terminal(observed))
                    | Event::HistoryUncertain(observed) => {
                        observed.thread_id == terminal.thread_id
                            && observed.turn_id == terminal.turn_id
                    }
                    _ => false,
                });
                if !delivered {
                    out.push(self.event(Some(key.clone()), Event::HistoryUncertain(terminal)));
                }
            }
        }
        result
    }
    fn apply_conversation_effects(
        &mut self,
        key: ConversationKey,
        events: Vec<conversation::Event>,
        cancel: &AtomicBool,
        out: &mut Vec<ScopedEvent>,
    ) -> Result<(), Fault> {
        for event in events {
            match &event {
                conversation::Event::ThreadReady { thread_id, .. } => {
                    if let Some(known) = self
                        .records
                        .get(&key)
                        .and_then(|record| record.thread_id.as_ref())
                    {
                        if known != thread_id {
                            return self.fail(Fault::PersistenceUnavailable);
                        }
                    } else {
                        self.persist(
                            &key,
                            durable::Call::Bind {
                                scope: self.association_scope,
                                conversation: key.0.clone(),
                                thread_id: thread_id.clone(),
                                expected_revision: self
                                    .records
                                    .get(&key)
                                    .map(|record| record.revision),
                            },
                        )?;
                    }
                    self.check_prompt_scope(cancel)?;
                    if self.prompt_cancelled() {
                        self.reject_local_intent()?;
                        self.closing_deadline = Some(Instant::now() + RPC);
                        self.maybe_unsubscribe(cancel)?;
                        out.push(self.event(Some(key.clone()), Event::Conversation(event)));
                        continue;
                    }
                    let id = self.id()?;
                    let command = self
                        .conversations
                        .get_mut(&key)
                        .ok_or(Fault::Protocol)?
                        .begin_turn(&self.scope, id)
                        .map_err(conversation_fault)?;
                    let deadline = self.prompt.as_ref().ok_or(Fault::Protocol)?.deadline;
                    self.send_command(command, deadline, cancel)?;
                }
                conversation::Event::InterruptRequired { .. } => {
                    let id = self.id()?;
                    let command = self
                        .conversations
                        .get_mut(&key)
                        .ok_or(Fault::Protocol)?
                        .begin_interrupt(&self.scope, id)
                        .map_err(conversation_fault)?;
                    self.send_command(command, Instant::now() + RPC, cancel)?;
                }
                conversation::Event::TurnStarted { turn_id } => {
                    let record = self
                        .records
                        .get(&key)
                        .ok_or(Fault::PersistenceUnavailable)?;
                    let reply = self.prompt.as_ref().ok_or(Fault::Protocol)?.reply;
                    self.persist(
                        &key,
                        durable::Call::RecordTurn {
                            scope: self.association_scope,
                            conversation: key.0.clone(),
                            expected_revision: record.revision,
                            reply,
                            turn_id: turn_id.clone(),
                        },
                    )?;
                }
                conversation::Event::Terminal(result) => {
                    let outcome = match result.outcome {
                        conversation::Outcome::Completed => durable::Terminal::Completed,
                        conversation::Outcome::Interrupted => durable::Terminal::Interrupted,
                        conversation::Outcome::Failed => durable::Terminal::Failed,
                        conversation::Outcome::Unknown => {
                            return self.fail(Fault::HistoryUncertain);
                        }
                    };
                    let record = self
                        .records
                        .get(&key)
                        .ok_or(Fault::PersistenceUnavailable)?;
                    let reply = self.prompt.as_ref().ok_or(Fault::Protocol)?.reply;
                    let settlement = self.persist(
                        &key,
                        durable::Call::Settle {
                            scope: self.association_scope,
                            conversation: key.0.clone(),
                            expected_revision: record.revision,
                            reply,
                            turn_id: result.turn_id.clone(),
                            outcome,
                        },
                    );
                    if settlement.is_err() {
                        out.push(
                            self.event(Some(key.clone()), Event::HistoryUncertain(result.clone())),
                        );
                        return Err(Fault::PersistenceUnavailable);
                    }
                    self.prompt = None;
                    self.closing_deadline = Some(Instant::now() + RPC);
                }
                conversation::Event::Closed { .. } => {
                    if self.prompt.as_ref().is_some_and(|p| p.unsent.is_some()) {
                        self.finish_unsent()?;
                        out.append(&mut self.ready_events);
                    } else {
                        self.active = None;
                        self.prompt = None;
                        self.closing_deadline = None;
                    }
                }
                _ => {}
            }
            out.push(self.event(Some(key.clone()), Event::Conversation(event)));
        }
        self.maybe_unsubscribe(cancel)
    }
    fn maybe_unsubscribe(&mut self, cancel: &AtomicBool) -> Result<(), Fault> {
        let Some(deadline) = self.closing_deadline else {
            return Ok(());
        };
        let Some(key) = self.active.clone() else {
            return Ok(());
        };
        // A terminal may race the interrupt acknowledgement. Do not release
        // ownership or unsubscribe until all of this turn's RPCs are settled.
        if self
            .pending
            .values()
            .any(|p| matches!(p.kind, PendingKind::Conversation(_)))
        {
            return Ok(());
        }
        if self.conversations.get(&key).ok_or(Fault::Protocol)?.phase() != Phase::Ready {
            return Ok(());
        }
        let id = self.id()?;
        let command = self
            .conversations
            .get_mut(&key)
            .ok_or(Fault::Protocol)?
            .begin_unsubscribe(&self.scope, id)
            .map_err(conversation_fault)?;
        self.send_command(command, deadline, cancel)
    }
    /// At most 32 frames per pump. Caller must keep pumping while idle so bounded
    /// pipe backpressure and original notifications are not mistaken for state.
    pub fn poll(&mut self, scope: &Scope, cancel: &AtomicBool) -> Result<Vec<ScopedEvent>, Fault> {
        self.current(scope)?;
        let mut observed = std::mem::take(&mut self.ready_events);
        match self.poll_inner(cancel, &mut observed) {
            Ok(()) => Ok(observed),
            Err(fault) => {
                // A failed connection must not deliver an undispatched browser
                // URL or re-authorize account/catalog readiness from this batch.
                observed.retain(|event| {
                    matches!(
                        event.event,
                        Event::Conversation(_)
                            | Event::HistoryUncertain(_)
                            | Event::NeverStarted { .. }
                    )
                });
                wire::preserve_observed(observed, &mut self.failure_events);
                self.fail(fault)
            }
        }
    }
    fn poll_inner(&mut self, cancel: &AtomicBool, out: &mut Vec<ScopedEvent>) -> Result<(), Fault> {
        let mut deadline = self.deadline;
        for pending in self.pending.values() {
            deadline = deadline.min(pending.deadline);
        }
        if let Some(login) = self.login_deadline {
            deadline = deadline.min(login);
        }
        if let Some(closing) = self.closing_deadline {
            deadline = deadline.min(closing);
        }
        if let Some(prompt) = &self.prompt {
            deadline = deadline.min(prompt.deadline);
            if prompt.text.is_some() {
                deadline = deadline.min(prompt.open_deadline);
            }
        }
        check(deadline, cancel)?;
        if self
            .prompt
            .as_ref()
            .and_then(|p| p.retry_at)
            .is_some_and(|at| Instant::now() >= at)
        {
            self.prompt.as_mut().ok_or(Fault::Protocol)?.retry_at = None;
            self.open_prompt(cancel)?;
        }
        for _ in 0..MAX_POLL_FRAMES {
            let Some(mut bytes) = self
                .session
                .as_mut()
                .ok_or(Fault::Closed)?
                .poll_frame(deadline, cancel)
                .map_err(transport)?
            else {
                break;
            };
            let incoming = wire::decode(&bytes);
            bytes.fill(0);
            match incoming.map_err(|f| {
                if f == wire::Fault::Unsupported {
                    Fault::UnsupportedAction
                } else {
                    Fault::Protocol
                }
            })? {
                wire::Incoming::Reply { id, result } => self.reply(id, result, cancel, out)?,
                wire::Incoming::Notification { method, params } => {
                    self.notification(&method, params, cancel, out)?
                }
            }
        }
        out.append(&mut self.ready_events);
        Ok(())
    }
    fn reply(
        &mut self,
        id: u64,
        result: Result<Value, Value>,
        cancel: &AtomicBool,
        out: &mut Vec<ScopedEvent>,
    ) -> Result<(), Fault> {
        let pending = self.pending.remove(&id).ok_or(Fault::Protocol)?;
        match pending.kind {
            PendingKind::Account => {
                let event = self
                    .account
                    .reply(&self.scope, id, result.map_err(|_| ()))
                    .map_err(|_| Fault::Protocol)?;
                self.account_effect(event, cancel, out)?;
            }
            PendingKind::Catalog(cursor) => {
                if self.state() != AccountState::ChatGpt {
                    return Err(Fault::NotConnected);
                }
                self.catalog_builder
                    .as_mut()
                    .ok_or(Fault::Protocol)?
                    .push_page(cursor.as_deref(), &result.map_err(|_| Fault::Upstream)?)
                    .map_err(|_| Fault::Protocol)?;
                if self
                    .catalog_builder
                    .as_ref()
                    .ok_or(Fault::Protocol)?
                    .next_cursor()
                    .is_some()
                {
                    self.catalog_page(pending.deadline, cancel)?;
                } else {
                    let catalog = self
                        .catalog_builder
                        .take()
                        .ok_or(Fault::Protocol)?
                        .finish()
                        .map_err(|_| Fault::Protocol)?;
                    out.push(self.event(None, Event::Catalog(catalog.clone())));
                    self.catalog = Some(catalog);
                }
            }
            PendingKind::Conversation(command) => {
                let key = self.active.clone().ok_or(Fault::Protocol)?;
                let result = match result {
                    Ok(value) => value,
                    Err(error) => {
                        if let Command::ResumeThread { thread_id, .. } = &command {
                            if wire::closing_resume(&error, thread_id) {
                                self.conversations
                                    .get_mut(&key)
                                    .ok_or(Fault::Protocol)?
                                    .resume_deferred(&self.scope, id)
                                    .map_err(conversation_fault)?;
                                self.prompt.as_mut().ok_or(Fault::Protocol)?.retry_at =
                                    Some(Instant::now() + Duration::from_millis(100));
                                return Ok(());
                            }
                        }
                        self.conversations
                            .get_mut(&key)
                            .ok_or(Fault::Protocol)?
                            .request_failed(&self.scope, id)
                            .map_err(conversation_fault)?;
                        return Err(Fault::Upstream);
                    }
                };
                if let Command::StartThread { choice, .. } | Command::ResumeThread { choice, .. } =
                    &command
                {
                    compatibility::validate_opened_thread(
                        &result,
                        path(self.runtime.cwd())?,
                        choice,
                    )
                    .map_err(|_| Fault::Policy)?;
                }
                match &command {
                    Command::StartThread { .. } => self
                        .advisory_join
                        .thread_reply(
                            result
                                .pointer("/thread/id")
                                .and_then(Value::as_str)
                                .ok_or(Fault::Protocol)?,
                        )
                        .map_err(|()| Fault::Protocol)?,
                    Command::StartTurn { thread_id, .. } => self
                        .advisory_join
                        .turn_reply(
                            thread_id,
                            result
                                .pointer("/turn/id")
                                .and_then(Value::as_str)
                                .ok_or(Fault::Protocol)?,
                        )
                        .map_err(|()| Fault::Protocol)?,
                    _ => (),
                }
                let events = self
                    .conversations
                    .get_mut(&key)
                    .ok_or(Fault::Protocol)?
                    .reply(&self.scope, id, &result)
                    .map_err(conversation_fault)?;
                self.conversation_effects(key, events, cancel, out)?;
            }
        }
        Ok(())
    }
    fn notification(
        &mut self,
        method: &str,
        params: Value,
        cancel: &AtomicBool,
        out: &mut Vec<ScopedEvent>,
    ) -> Result<(), Fault> {
        use wire::NotificationClass;
        self.notification_frames = self
            .notification_frames
            .checked_add(1)
            .ok_or(Fault::Limit)?;
        if self.notification_frames > MAX_NOTIFICATION_FRAMES {
            return Err(Fault::Limit);
        }
        let classified =
            wire::classify_notification(method, &params).map_err(|fault| match fault {
                wire::Fault::Unsupported => Fault::UnsupportedAction,
                wire::Fault::Policy => Fault::Policy,
                wire::Fault::Envelope => Fault::Protocol,
            })?;
        match classified {
            NotificationClass::AccountInvalidation | NotificationClass::AccountLoginCompleted => {
                let notification = if classified == NotificationClass::AccountInvalidation {
                    managed_account::Notification::AccountUpdated
                } else {
                    managed_account::Notification::LoginCompleted
                };
                let event = self
                    .account
                    .notification(&self.scope, notification, params)
                    .map_err(|_| Fault::Protocol)?;
                return self.account_effect(event, cancel, out);
            }
            NotificationClass::RemoteAuthority => return Err(Fault::Policy),
            NotificationClass::Informational(advisory) => {
                // Inert metadata can arrive before its RPC reply or after a
                // settled terminal. Join only to retained native ownership;
                // it cannot create a thread, authenticate or finish a call.
                let threads: Vec<_> = self
                    .conversations
                    .values()
                    .filter_map(Conversation::thread_id)
                    .collect();
                let mut turns: Vec<_> = self
                    .conversations
                    .values()
                    .filter_map(|conversation| {
                        Some((conversation.thread_id()?, conversation.active_turn_id()?))
                    })
                    .collect();
                turns.extend(
                    self.settled_turns
                        .values()
                        .map(|(thread, turn)| (thread.as_str(), turn.as_str())),
                );
                let starting_thread = self.pending.values().any(|pending| {
                    matches!(
                        pending.kind,
                        PendingKind::Conversation(Command::StartThread { .. })
                    )
                });
                let starting_turn = self
                    .pending
                    .values()
                    .find_map(|pending| match &pending.kind {
                        PendingKind::Conversation(Command::StartTurn { thread_id, .. }) => {
                            Some(thread_id.as_str())
                        }
                        _ => None,
                    });
                return self
                    .advisory_join
                    .accept(
                        advisory.scope,
                        &threads,
                        &turns,
                        starting_thread,
                        starting_turn,
                    )
                    .map_err(|()| Fault::Protocol);
            }
            NotificationClass::ConversationUsage(wire::AdvisoryScope::Turn {
                thread_id,
                turn_id,
            }) => {
                // Original usage may trail its authoritative terminal, even
                // after the next Begin replaced the durable row. Known-tail
                // metadata is not new spend, a new turn or completion proof.
                if self
                    .settled_turns
                    .values()
                    .any(|(thread, turn)| thread == thread_id && turn == turn_id)
                {
                    return Ok(());
                }
            }
            NotificationClass::ConversationUsage(_) => return Err(Fault::Protocol),
            NotificationClass::Conversation => (),
        }
        let Some(key) = self.active.clone() else {
            return Err(Fault::Protocol);
        };
        let events = self
            .conversations
            .get_mut(&key)
            .ok_or(Fault::Protocol)?
            .notification(&self.scope, method, &params)
            .map_err(conversation_fault)?;
        self.conversation_effects(key, events, cancel, out)
    }
    fn remember_settled(&mut self, key: &ConversationKey, record: &durable::Association) {
        if let durable::Dispatch::Settled { turn_id, .. } = &record.dispatch {
            // One last terminal per bounded owned conversation survives Begin
            // replacing its Store row. Delayed inert tails are not new turns.
            if let Some(thread) = &record.thread_id {
                self.settled_turns
                    .insert(key.clone(), (thread.clone(), turn_id.clone()));
            }
        }
    }
    fn validate_record(
        &self,
        key: &ConversationKey,
        record: &durable::Association,
    ) -> Result<(), Fault> {
        if record.scope != self.association_scope
            || record.conversation != key.0
            || record.revision == 0
            || record.revision > i64::MAX as u64
            || match &record.thread_id {
                Some(thread) => !durable::valid_provider_id(thread),
                None => !matches!(record.dispatch, durable::Dispatch::Skipped { .. }),
            }
        {
            return Err(Fault::PersistenceUnavailable);
        }
        Ok(())
    }
    fn persist(&mut self, key: &ConversationKey, call: durable::Call) -> Result<(), Fault> {
        let previous = self.records.get(key).cloned();
        let prior_revision = previous.as_ref().map(|r| r.revision);
        if self
            .prompt
            .as_ref()
            .is_some_and(|p| p.key == *key && p.acknowledged_revision != prior_revision)
        {
            return self.fail(Fault::PersistenceUnavailable);
        }
        let call_revision = match &call {
            durable::Call::Bind {
                expected_revision, ..
            }
            | durable::Call::Skip {
                expected_revision, ..
            } => *expected_revision,
            durable::Call::Begin {
                expected_revision, ..
            }
            | durable::Call::RecordTurn {
                expected_revision, ..
            }
            | durable::Call::Settle {
                expected_revision, ..
            } => Some(*expected_revision),
            _ => return self.fail(Fault::PersistenceUnavailable),
        };
        if call_revision != prior_revision {
            return self.fail(Fault::PersistenceUnavailable);
        }
        let expected = call.clone();
        let record = match self.associations.call(call) {
            durable::Reply::Association(Some(r)) => r,
            _ => return self.fail(Fault::PersistenceUnavailable),
        };
        let next = prior_revision.unwrap_or(0).checked_add(1);
        let exact = Some(record.revision) == next
            && match expected {
                durable::Call::Bind { thread_id, .. } => {
                    record.thread_id.as_ref() == Some(&thread_id)
                        && match &previous {
                            None => record.dispatch == durable::Dispatch::Ready,
                            Some(prior) => {
                                prior.thread_id.is_none()
                                    && matches!(prior.dispatch, durable::Dispatch::Skipped { .. })
                                    && record.dispatch == prior.dispatch
                            }
                        }
                }
                durable::Call::Skip { reply, .. } => {
                    record.thread_id == previous.as_ref().and_then(|p| p.thread_id.clone())
                        && record.dispatch == durable::Dispatch::Skipped { reply }
                }
                durable::Call::Begin { reply, .. } => {
                    record.dispatch
                        == durable::Dispatch::Pending {
                            reply,
                            turn_id: None,
                        }
                }
                durable::Call::RecordTurn { reply, turn_id, .. } => {
                    record.dispatch
                        == durable::Dispatch::Pending {
                            reply,
                            turn_id: Some(turn_id),
                        }
                }
                durable::Call::Settle {
                    reply,
                    turn_id,
                    outcome,
                    ..
                } => {
                    record.dispatch
                        == durable::Dispatch::Settled {
                            reply,
                            turn_id,
                            outcome,
                        }
                }
                _ => false,
            };
        if !exact || self.validate_record(key, &record).is_err() {
            return self.fail(Fault::PersistenceUnavailable);
        }
        if previous
            .as_ref()
            .and_then(|p| p.thread_id.as_ref())
            .is_some_and(|thread| record.thread_id.as_ref() != Some(thread))
        {
            return self.fail(Fault::PersistenceUnavailable);
        }
        if let Some(prompt) = self.prompt.as_mut().filter(|p| p.key == *key) {
            prompt.acknowledged_revision = Some(record.revision);
        }
        self.remember_settled(key, &record);
        self.records.insert(key.clone(), record);
        Ok(())
    }
    fn event(&self, conversation: Option<ConversationKey>, event: Event) -> ScopedEvent {
        ScopedEvent {
            scope: self.scope.clone(),
            conversation,
            event,
        }
    }
    pub fn take_failure_events(&mut self, scope: &Scope) -> Result<Vec<ScopedEvent>, Fault> {
        if scope != &self.scope {
            return Err(Fault::ScopeChanged);
        }
        Ok(std::mem::take(&mut self.failure_events))
    }
    fn fail<T>(&mut self, fault: Fault) -> Result<T, Fault> {
        if let Some(key) = self.active.take() {
            if let Some(conversation) = self.conversations.get_mut(&key) {
                if let Ok(events) = conversation.transport_lost(&self.scope) {
                    for event in events {
                        self.failure_events.push(ScopedEvent {
                            scope: self.scope.clone(),
                            conversation: Some(key.clone()),
                            event: Event::Conversation(event),
                        });
                    }
                }
            }
        }
        self.catalog = None;
        self.catalog_builder = None;
        self.prompt = None;
        self.pending.clear();
        if fault == Fault::Timeout {
            let _ = self.account.timeout(&self.scope);
        }
        let _ = self.account.close(&self.scope);
        // Dropping Session blocks until original process/job/writer settle.
        drop(self.session.take());
        Err(fault)
    }
    pub fn close(mut self) -> Result<(), Fault> {
        let _ = self.account.close(&self.scope);
        let session = self.session.take().ok_or(Fault::Closed)?;
        let exit = session.finish(Instant::now() + CLOSE).map_err(transport)?;
        if exit.code != 0 {
            return Err(Fault::Transport);
        }
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        drop(self.session.take());
    }
}
fn command_id(command: &Command) -> u64 {
    match command {
        Command::StartThread { request_id, .. }
        | Command::ResumeThread { request_id, .. }
        | Command::StartTurn { request_id, .. }
        | Command::Interrupt { request_id, .. }
        | Command::Unsubscribe { request_id, .. } => *request_id,
    }
}
fn path(path: &std::path::Path) -> Result<&str, Fault> {
    path.to_str().ok_or(Fault::Profile)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn check(deadline: Instant, cancel: &AtomicBool) -> Result<(), Fault> {
    if cancel.load(Ordering::Acquire) {
        Err(Fault::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Fault::Timeout)
    } else {
        Ok(())
    }
}
fn discovery_fault(f: discovery::DiscoveryFault) -> Fault {
    match f {
        discovery::DiscoveryFault::NotFound | discovery::DiscoveryFault::Unsupported => {
            Fault::UnsupportedRuntime
        }
        discovery::DiscoveryFault::Cancelled => Fault::Cancelled,
        discovery::DiscoveryFault::Timeout => Fault::Timeout,
        discovery::DiscoveryFault::Limit => Fault::Limit,
        _ => Fault::RuntimeUnavailable,
    }
}
fn transport(f: windows::Fault) -> Fault {
    match f {
        windows::Fault::Cancelled => Fault::Cancelled,
        windows::Fault::Timeout => Fault::Timeout,
        _ => Fault::Transport,
    }
}
fn conversation_fault(f: conversation::Fault) -> Fault {
    match f {
        conversation::Fault::ScopeChanged => Fault::ScopeChanged,
        conversation::Fault::Busy | conversation::Fault::State => Fault::Busy,
        conversation::Fault::Limit => Fault::Limit,
        conversation::Fault::UnsupportedAction => Fault::UnsupportedAction,
        conversation::Fault::Upstream => Fault::Upstream,
        _ => Fault::Protocol,
    }
}
fn probe_session(
    image: Arc<crate::PinnedImage>,
    runtime: Arc<RuntimePaths>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(), Fault> {
    check(deadline, cancel)?;
    let scope = Scope::new(
        "credential-free-compatibility".into(),
        random_id().map_err(|_| Fault::Profile)?,
        1,
    )
    .map_err(|_| Fault::ScopeChanged)?;
    let projection = image.projection();
    let mut session = Session::spawn(image, runtime.clone()).map_err(transport)?;
    let mut account =
        policy_handshake(&mut session, &runtime, &scope, projection, deadline, cancel)?;
    let read = serde_json::json!({"id":6,"method":"account/read","params":{"refreshToken":false}});
    session
        .send_frame(
            &serde_json::to_vec(&read).map_err(|_| Fault::Protocol)?,
            deadline,
            cancel,
        )
        .map_err(transport)?;
    let result = startup_receive(
        &mut session,
        &mut account,
        &scope,
        Some(6),
        &mut 0,
        deadline,
        cancel,
    )?
    .ok_or(Fault::Protocol)?;
    protocol::accept_account(&result).map_err(|_| Fault::NotConnected)?;
    let exit = session
        .finish(deadline.min(Instant::now() + CLOSE))
        .map_err(transport)?;
    if exit.code != 0 {
        return Err(Fault::Transport);
    }
    Ok(())
}
fn policy_handshake(
    session: &mut Session,
    runtime: &RuntimePaths,
    scope: &Scope,
    projection: ConfigProjection,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<ManagedAccount, Fault> {
    let initialized = exchange(session, wire::initialize(), 1, deadline, cancel)?;
    compatibility::validate_initialize(&initialized, path(runtime.home())?)
        .map_err(|_| Fault::Policy)?;
    let receipt = InitializedAccountReceipt::from_native_handshake(scope.clone(), 1, initialized)
        .map_err(|_| Fault::Protocol)?;
    let mut account = ManagedAccount::from_initialized(receipt).map_err(|_| Fault::Protocol)?;
    session
        .send_frame(&protocol::initialized(), deadline, cancel)
        .map_err(transport)?;
    let mut startup_frames = 0;
    startup_receive(
        session,
        &mut account,
        scope,
        None,
        &mut startup_frames,
        deadline,
        cancel,
    )?;
    session
        .send_frame(&protocol::config(path(runtime.cwd())?), deadline, cancel)
        .map_err(transport)?;
    let config = startup_receive(
        session,
        &mut account,
        scope,
        Some(3),
        &mut startup_frames,
        deadline,
        cancel,
    )?
    .ok_or(Fault::Protocol)?;
    compatibility::validate_startup_config(
        &config,
        path(&runtime.home().join("config.toml"))?,
        projection,
    )
    .map_err(|_| Fault::Policy)?;
    session
        .send_frame(&protocol::requirements(), deadline, cancel)
        .map_err(transport)?;
    let requirements = startup_receive(
        session,
        &mut account,
        scope,
        Some(4),
        &mut startup_frames,
        deadline,
        cancel,
    )?
    .ok_or(Fault::Protocol)?;
    compatibility::validate_requirements(&requirements).map_err(|_| Fault::Policy)?;
    let feature_request = compatibility::feature_request(5).map_err(|_| Fault::Policy)?;
    session
        .send_frame(
            &serde_json::to_vec(&feature_request).map_err(|_| Fault::Protocol)?,
            deadline,
            cancel,
        )
        .map_err(transport)?;
    let features = startup_receive(
        session,
        &mut account,
        scope,
        Some(5),
        &mut startup_frames,
        deadline,
        cancel,
    )?
    .ok_or(Fault::Protocol)?;
    compatibility::validate_features(&features, projection).map_err(|_| Fault::Policy)?;
    Ok(account)
}

fn exchange(
    session: &mut Session,
    request: Vec<u8>,
    id: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Value, Fault> {
    session
        .send_frame(&request, deadline, cancel)
        .map_err(transport)?;
    let mut bytes = session.recv_frame(deadline, cancel).map_err(transport)?;
    let result = protocol::reply(&bytes, id).map_err(|_| Fault::Protocol);
    bytes.fill(0);
    result
}

// Signed-in original startup may emit account/updated while configuration reads
// are outstanding. It invalidates only; the later fresh account/read is mandatory.
fn startup_receive(
    session: &mut Session,
    account: &mut ManagedAccount,
    scope: &Scope,
    expected: Option<u64>,
    frames: &mut usize,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Option<Value>, Fault> {
    loop {
        *frames = frames.checked_add(1).ok_or(Fault::Limit)?;
        if *frames > 64 {
            return Err(Fault::Limit);
        }
        let mut bytes = session.recv_frame(deadline, cancel).map_err(transport)?;
        let frame = wire::startup(&bytes, expected);
        bytes.fill(0);
        match frame.map_err(|_| Fault::Protocol)? {
            wire::Startup::RemoteDisabled => return Ok(None),
            wire::Startup::Reply(value) => return Ok(Some(value)),
            wire::Startup::Informational(kind) => {
                if !matches!(
                    kind,
                    wire::AdvisoryKind::RateLimits
                        | wire::AdvisoryKind::Warning
                        | wire::AdvisoryKind::Deprecation
                        | wire::AdvisoryKind::SkillsChanged
                ) {
                    return Err(Fault::Protocol);
                }
            }
            wire::Startup::AccountInvalidation(params) => {
                let event = account
                    .notification(scope, managed_account::Notification::AccountUpdated, params)
                    .map_err(|_| Fault::Protocol)?;
                if event.scope() != scope || !event.invalidates_admission() {
                    return Err(Fault::Protocol);
                }
                if !matches!(
                    event.into_effect(),
                    Effect::Ignored | Effect::Changed(AccountState::Unknown)
                ) {
                    return Err(Fault::Protocol);
                }
            }
        }
    }
}

fn managed_fault(fault: managed_account::Fault) -> Fault {
    use zephium_provider_accounts::codex::Fault as AccountFault;
    match fault {
        managed_account::Fault::ScopeChanged => Fault::ScopeChanged,
        managed_account::Fault::Account(AccountFault::Busy) | managed_account::Fault::State => {
            Fault::Busy
        }
        managed_account::Fault::Account(
            AccountFault::ConnectionClosed | AccountFault::NotInitialized,
        ) => Fault::Closed,
        managed_account::Fault::Account(AccountFault::TimedOut) => Fault::Timeout,
        managed_account::Fault::Account(AccountFault::ProviderRefused) => Fault::Upstream,
        _ => Fault::Protocol,
    }
}

#[cfg(all(debug_assertions, feature = "native-fixtures"))]
pub(crate) mod fixture_peer;
#[cfg(all(debug_assertions, feature = "native-fixtures"))]
pub(crate) mod fixtures;
