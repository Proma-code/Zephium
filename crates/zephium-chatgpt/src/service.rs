//! Per-profile OAuth lifecycle, serialized token rotation and cancellation fences.
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{watch, Mutex, Notify};
use zeroize::Zeroizing;

use crate::inference::{InferenceClient, Model};
use crate::oauth::{AuthError, Authorization, Client, LoginAttempt};
use crate::token::{Identity, Tokens};
use crate::vault::{AccountVault, PendingRegistration, Stored, VaultError};
use zephium_core::work::model::{
    ChatGptModelFault, WorkModelError, WorkModelEvent, WorkModelOutcome, WorkModelRequest,
};

/// User callback deadline, not reset by invalid callback traffic.
/// An accepted one-use token exchange finishes under its own HTTP deadline.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

/// Closed, non-secret account faults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// Invalid configuration or profile identity.
    Invalid,
    /// The provider could not be reached.
    Offline,
    /// Authorization was denied or did not grant plan usage.
    Denied,
    /// The person cancelled the pending sign-in.
    Cancelled,
    /// A bounded operation timed out.
    Timeout,
    /// A callback, token or response violated the expected protocol.
    Protocol,
    /// Native protected storage refused the operation.
    Vault,
    /// The renewable session requires a new sign-in.
    SessionExpired,
    /// The provider reports a usage/rate limit.
    UsageLimit,
    /// Another sign-in or mutation is pending.
    Busy,
}

impl From<AuthError> for Fault {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Configuration | AuthError::Random | AuthError::Clock => Self::Invalid,
            AuthError::AccessDenied | AuthError::Scope => Self::Denied,
            AuthError::Transport => Self::Offline,
            AuthError::Timeout => Self::Timeout,
            AuthError::RefreshExpired | AuthError::RefreshUncertain | AuthError::OAuthRejected => {
                Self::SessionExpired
            }
            AuthError::RateLimited => Self::UsageLimit,
            _ => Self::Protocol,
        }
    }
}

/// Non-secret native projection. Connected is not an entitlement guarantee.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// A verified registration has a locally stored renewable session.
    pub connected: bool,
    /// A separate new authorization attempt is pending.
    pub signing_in: bool,
    /// Account label from the validated identity token.
    pub email: Option<String>,
    /// Number of conservatively supported models in the current catalog.
    pub models: usize,
    /// Exact pending operation correlation, not an OAuth state or credential.
    pub login_attempt: Option<String>,
    /// Closed result of the latest user operation.
    pub fault: Option<Fault>,
    /// Local sign-out finished without confirming remote revocation.
    pub revocation_unconfirmed: bool,
}

#[derive(Default)]
struct State {
    loaded: bool,
    stopped: bool,
    foreground: bool,
    session: u64,
    identity: Option<Identity>,
    pending_registration: Option<PendingRegistration>,
    cleanup_pending: bool,
    tokens: Option<Arc<Tokens>>,
    pending_rotation: Option<Arc<Tokens>>,
    pending_revocations: Vec<Arc<Tokens>>,
    revocation_lost: bool,
    models: Vec<Model>,
    signing_in: bool,
    attempt: Option<String>,
    cancelled_attempts: VecDeque<String>,
    generation: u64,
    fault: Option<Fault>,
    revocation_unconfirmed: bool,
}

type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// Fixed production transport; the private seam permits deterministic offline
// lifecycle tests without a configurable token/inference endpoint in the product.
trait Transport: Send + Sync {
    fn exchange(&self, authorization: Authorization) -> PortFuture<'_, Result<Tokens, AuthError>>;
    fn refresh<'a>(&'a self, tokens: &'a Tokens) -> PortFuture<'a, Result<Tokens, AuthError>>;
    fn revoke<'a>(&'a self, tokens: &'a Tokens) -> PortFuture<'a, Result<(), AuthError>>;
    fn models<'a>(
        &'a self,
        tokens: &'a Tokens,
    ) -> PortFuture<'a, Result<Vec<Model>, WorkModelError>>;
    fn call<'a>(
        &'a self,
        tokens: &'a Tokens,
        request: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> PortFuture<'a, Result<WorkModelOutcome, WorkModelError>>;
}
struct Official {
    oauth: Client,
    inference: InferenceClient,
}
impl Transport for Official {
    fn exchange(&self, authorization: Authorization) -> PortFuture<'_, Result<Tokens, AuthError>> {
        Box::pin(self.oauth.exchange(authorization))
    }
    fn refresh<'a>(&'a self, tokens: &'a Tokens) -> PortFuture<'a, Result<Tokens, AuthError>> {
        Box::pin(self.oauth.refresh(tokens))
    }
    fn revoke<'a>(&'a self, tokens: &'a Tokens) -> PortFuture<'a, Result<(), AuthError>> {
        Box::pin(self.oauth.revoke(tokens))
    }
    fn models<'a>(
        &'a self,
        tokens: &'a Tokens,
    ) -> PortFuture<'a, Result<Vec<Model>, WorkModelError>> {
        Box::pin(self.inference.models(tokens))
    }
    fn call<'a>(
        &'a self,
        tokens: &'a Tokens,
        request: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> PortFuture<'a, Result<WorkModelOutcome, WorkModelError>> {
        Box::pin(self.inference.call(tokens, request, events))
    }
}

#[derive(Default)]
struct TaskState {
    closed: bool,
    active: usize,
}
#[derive(Default)]
struct Tasks {
    state: StdMutex<TaskState>,
    changed: Notify,
}
struct Ticket(Arc<Tasks>);
impl Drop for Ticket {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active -= 1;
        self.0.changed.notify_waiters();
    }
}

/// Native-only authorization lease. A queued Work client cannot cross sign-out
/// or a replacement sign-in; token refresh does not change this lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lease(u64);

/// One browser profile's account. Never shared with another profile or Cloud.
pub struct Service {
    profile: String,
    host_id: String,
    transport: Arc<dyn Transport>,
    vault: Arc<dyn AccountVault>,
    mutation: Mutex<()>,
    state: RwLock<State>,
    cancellation: watch::Sender<u64>,
    changed: Arc<dyn Fn() + Send + Sync>,
    tasks: Arc<Tasks>,
}

impl Service {
    /// Construct without reading storage or contacting any service.
    pub fn new(
        profile: String,
        host_id: String,
        vault: Arc<dyn AccountVault>,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, Fault> {
        if zephium_core::ids::ProfileId::parse(&profile).is_none_or(|id| id.to_string() != profile)
            || !crate::token::valid_host(&host_id)
        {
            return Err(Fault::Invalid);
        }
        let (cancellation, _) = watch::channel(0);
        Ok(Self {
            profile,
            host_id,
            transport: Arc::new(Official {
                oauth: Client::new()?,
                inference: InferenceClient::new().map_err(model_fault)?,
            }),
            vault,
            mutation: Mutex::new(()),
            state: RwLock::new(State::default()),
            cancellation,
            changed,
            tasks: Arc::new(Tasks::default()),
        })
    }

    // A ticket stays inside the owned task, not the caller's join future.
    // Dropping IPC/inference waiters cannot drop a refresh or protected write.
    fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Result<tokio::task::JoinHandle<T>, Fault> {
        let mut tasks = self.tasks.state.lock().unwrap_or_else(|e| e.into_inner());
        if tasks.closed {
            return Err(Fault::Cancelled);
        }
        if tasks.active >= 64 {
            return Err(Fault::Busy);
        }
        tasks.active += 1;
        let ticket = Ticket(self.tasks.clone());
        Ok(tokio::spawn(async move {
            let _ticket = ticket;
            future.await
        }))
    }

    /// Close task admission and stop output. Only app shutdown uses this; a
    /// normal sign-out remains reversible by another explicit sign-in.
    pub fn close(&self) {
        {
            let mut tasks = self.tasks.state.lock().unwrap_or_else(|e| e.into_inner());
            if tasks.closed {
                return;
            }
            tasks.closed = true;
        }
        self.set_foreground(false);
        self.stop();
    }

    /// Whether a retired owner has no outstanding task obligation. The native
    /// registry may release it, never before pending writes settle.
    pub fn closed_and_drained(&self) -> bool {
        let tasks = self.tasks.state.lock().unwrap_or_else(|e| e.into_inner());
        tasks.closed && tasks.active == 0
    }

    /// Wait for owned mutations to settle, without claiming success on timeout.
    /// The host applies its existing overall shutdown deadline around this future.
    pub async fn drained(&self) {
        loop {
            let changed = self.tasks.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .tasks
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active
                == 0
            {
                return;
            }
            changed.await;
        }
    }

    /// Erase only this profile's local protected account during an authorized
    /// profile deletion. The owned mutation finishes even if a deadline waiter
    /// drops; a later attempt can safely retry partial native deletion.
    pub async fn erase(self: &Arc<Self>) -> Result<(), Fault> {
        self.close();
        let ticket = {
            let mut tasks = self.tasks.state.lock().unwrap_or_else(|e| e.into_inner());
            if tasks.active >= 64 {
                return Err(Fault::Busy);
            }
            tasks.active += 1;
            Ticket(self.tasks.clone())
        };
        let service = self.clone();
        tokio::spawn(async move {
            let _ticket = ticket;
            let _guard = service.mutation.lock().await;
            let vault = service.vault.clone();
            let profile = service.profile.clone();
            tokio::task::spawn_blocking(move || vault.forget(&profile))
                .await
                .map_err(|_| Fault::Vault)?
                .map_err(|_| Fault::Vault)?;
            let mut state = service.state.write().unwrap_or_else(|e| e.into_inner());
            state.tokens = None;
            state.pending_rotation = None;
            state.identity = None;
            state.pending_registration = None;
            state.pending_revocations.clear();
            state.models.clear();
            state.loaded = true;
            state.fault = None;
            state.cleanup_pending = false;
            Ok(())
        })
        .await
        .map_err(|_| Fault::Vault)?
    }

    /// Current synchronous projection; it does not initiate network or vault I/O.
    pub fn view(&self) -> View {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        View {
            connected: !state.stopped
                && state.foreground
                && state.pending_rotation.is_none()
                && state.tokens.is_some(),
            signing_in: state.signing_in,
            email: state
                .identity
                .as_ref()
                .and_then(|identity| identity.email.clone()),
            models: state.models.len(),
            login_attempt: state.attempt.clone(),
            fault: state
                .fault
                .or(state.cleanup_pending.then_some(Fault::Vault)),
            revocation_unconfirmed: state.revocation_unconfirmed,
        }
    }

    /// Account catalog only while this profile has an active session.
    pub fn models(&self) -> Vec<Model> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if !state.stopped
            && state.foreground
            && state.pending_rotation.is_none()
            && state.tokens.is_some()
        {
            state.models.clone()
        } else {
            Vec::new()
        }
    }

    fn failed(&self, fault: Fault) {
        self.state.write().unwrap_or_else(|e| e.into_inner()).fault = Some(fault);
        (self.changed)();
    }

    /// Load the profile's exact vault record on explicit account observation.
    pub async fn load(self: &Arc<Self>) -> Result<(), Fault> {
        let service = self.clone();
        self.spawn(async move { service.load_owned().await })?
            .await
            .map_err(|_| Fault::Vault)?
    }

    async fn load_owned(&self) -> Result<(), Fault> {
        let _guard = self.mutation.lock().await;
        if self.state.read().unwrap_or_else(|e| e.into_inner()).loaded {
            return Ok(());
        }
        let profile = self.profile.clone();
        let vault = self.vault.clone();
        let loaded = tokio::task::spawn_blocking(move || vault.load(&profile))
            .await
            .map_err(|_| Fault::Vault)?;
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        match loaded {
            Ok(stored) => {
                if stored
                    .identity
                    .as_ref()
                    .is_some_and(|identity| identity.host_id != self.host_id)
                    || stored
                        .pending_registration
                        .as_ref()
                        .is_some_and(|pending| pending.host_id != self.host_id)
                {
                    return Err(Fault::Vault);
                }
                let tokens = stored
                    .tokens
                    .map(|bytes| Tokens::from_bytes(&bytes))
                    .transpose()
                    .map_err(|_| Fault::Vault)?;
                if tokens
                    .as_ref()
                    .is_some_and(|tokens| Some(tokens.identity()) != stored.identity.as_ref())
                {
                    return Err(Fault::Vault);
                }
                state.identity = stored.identity;
                state.pending_registration = stored.pending_registration;
                state.tokens = tokens.map(Arc::new);
                state.loaded = true;
            }
            Err(VaultError::Missing) => state.loaded = true,
            Err(_) => {
                state.fault = Some(Fault::Vault);
                return Err(Fault::Vault);
            }
        }
        Ok(())
    }

    async fn save(
        &self,
        identity: Option<Identity>,
        tokens: Option<&Tokens>,
        pending_registration: Option<PendingRegistration>,
    ) -> Result<(), Fault> {
        let record = Stored {
            identity,
            pending_registration,
            tokens: tokens
                .map(Tokens::to_bytes)
                .transpose()
                .map_err(Fault::from)?,
        };
        let profile = self.profile.clone();
        let vault = self.vault.clone();
        let outcome = tokio::task::spawn_blocking(move || vault.save(&profile, &record))
            .await
            .map_err(|_| Fault::Vault)?
            .map_err(|_| Fault::Vault)?;
        // Ok means the new manifest IS committed. Residual old-chunk deletion
        // failure must not make a rotating refresh fall back to obsolete tokens.
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .cleanup_pending = outcome.cleanup_pending;
        Ok(())
    }

    async fn remember_registration(&self, generation: u64, client_id: &str) -> Result<(), Fault> {
        let _guard = self.mutation.lock().await;
        let (identity, tokens) = {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            if state.generation != generation {
                return Err(Fault::Cancelled);
            }
            if state.identity.is_some() {
                return Ok(());
            }
            (state.identity.clone(), state.tokens.clone())
        };
        let pending =
            PendingRegistration::new(&self.host_id, client_id).map_err(|_| Fault::Protocol)?;
        self.save(identity, tokens.as_deref(), Some(pending.clone()))
            .await?;
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .pending_registration = Some(pending);
        Ok(())
    }

    /// Begin only from a fresh native user action. The host opens the system browser.
    /// The listener is bound before that browser opens; no authorization URL crosses IPC.
    pub async fn sign_in<F, Fut>(
        self: &Arc<Self>,
        attempt_id: String,
        open_browser: F,
    ) -> Result<(), Fault>
    where
        F: FnOnce(Zeroizing<String>) -> Fut + Send + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        let admitted = {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            if state.signing_in {
                return Err(Fault::Busy);
            }
            state.generation
        };
        let service = self.clone();
        self.spawn(async move {
            service
                .sign_in_owned(admitted, attempt_id, open_browser)
                .await
        })?
        .await
        .map_err(|_| Fault::Protocol)?
    }

    async fn sign_in_owned<F, Fut>(
        self: &Arc<Self>,
        admitted: u64,
        attempt_id: String,
        open_browser: F,
    ) -> Result<(), Fault>
    where
        F: FnOnce(Zeroizing<String>) -> Fut + Send + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        if !valid_attempt(&attempt_id) {
            return Err(Fault::Invalid);
        }
        self.load_owned().await?;
        let guard = self.mutation.lock().await;
        if self
            .state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .cancelled_attempts
            .contains(&attempt_id)
        {
            return Err(Fault::Cancelled);
        }
        {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            if !state.foreground || state.generation != admitted {
                return Err(Fault::Cancelled);
            }
        }
        if self.view().signing_in {
            return Err(Fault::Busy);
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| Fault::Offline)?;
        let port = listener.local_addr().map_err(|_| Fault::Offline)?.port();
        let (identity, pending) = {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            (state.identity.clone(), state.pending_registration.clone())
        };
        let attempt = if identity.is_none() {
            match pending {
                Some(pending) => {
                    LoginAttempt::retry_registration(&self.host_id, &pending.client_id, port)?
                }
                None => LoginAttempt::new(&self.host_id, None, port)?,
            }
        } else {
            LoginAttempt::new(&self.host_id, identity.as_ref(), port)?
        };
        let authorization_url = attempt.authorization_url()?;
        let generation = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if !state.foreground || state.generation != admitted {
                return Err(Fault::Cancelled);
            }
            state.generation = state.generation.checked_add(1).ok_or(Fault::Invalid)?;
            state.signing_in = true;
            state.attempt = Some(attempt_id.clone());
            state.fault = None;
            state.generation
        };
        let mut cancellation = self.cancellation.subscribe();
        drop(guard);
        (self.changed)();
        let opened = tokio::select! {
            biased;
            _ = cancellation.changed() => false,
            result = tokio::time::timeout(Duration::from_secs(10), open_browser(authorization_url)) => result.unwrap_or(false),
        };
        if !opened {
            self.cancel(&attempt_id).await;
            return Err(Fault::Offline);
        }
        if cancellation.has_changed().unwrap_or(true) {
            return Err(Fault::Cancelled);
        }
        let service = self.clone();
        let spawned = self.spawn(async move {
            let callback = tokio::select! {
                biased;
                _ = cancellation.changed() => Err(Fault::Cancelled),
                result = tokio::time::timeout(SIGN_IN_TIMEOUT, callback(&listener, &attempt)) => result.unwrap_or(Err(Fault::Timeout)),
            };
            drop(listener);
            let result = match callback {
                Err(fault) => Err(fault),
                Ok(callback) => match attempt.accept_callback(&callback).map_err(Fault::from) {
                    Err(fault) => Err(fault),
                    Ok(authorization) => {
                        // This owned task retains the mutation lock through the
                        // protected write even if its original IPC caller leaves.
                        match service.remember_registration(generation, authorization.client_id()).await {
                            Err(fault) => Err(fault),
                            Ok(()) if cancellation.has_changed().unwrap_or(true) => Err(Fault::Cancelled),
                            Ok(()) => service.transport.exchange(authorization).await.map_err(Fault::from),
                        }
                    }
                }
            };
            service.finish_login(generation, result).await;
        });
        if let Err(fault) = spawned {
            self.cancel(&attempt_id).await;
            return Err(fault);
        }
        Ok(())
    }

    async fn finish_login(self: &Arc<Self>, generation: u64, result: Result<Tokens, Fault>) {
        let guard = self.mutation.lock().await;
        if self
            .state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .generation
            != generation
        {
            if let Ok(tokens) = result {
                self.retire_issued(Arc::new(tokens)).await;
            }
            return;
        }
        let (tokens, result) = match result {
            Ok(tokens) => {
                let tokens = Arc::new(tokens);
                let saved = self
                    .save(Some(tokens.identity().clone()), Some(&tokens), None)
                    .await;
                (Some(tokens), saved)
            }
            Err(fault) => (None, Err(fault)),
        };
        // Cancellation can retire ownership during a protected commit. Roll
        // back the local projection before acknowledging it, and retain/revoke
        // the newly issued remote grant rather than silently dropping it.
        if self
            .state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .generation
            != generation
        {
            if let Some(tokens) = tokens {
                if result.is_ok() {
                    let (identity, old, pending) = {
                        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
                        (
                            state.identity.clone(),
                            state.tokens.clone(),
                            state.pending_registration.clone().or_else(|| {
                                PendingRegistration::new(
                                    &self.host_id,
                                    &tokens.identity().client_id,
                                )
                                .ok()
                            }),
                        )
                    };
                    if self.save(identity, old.as_deref(), pending).await.is_err() {
                        self.failed(Fault::Vault);
                    }
                }
                self.retire_issued(tokens).await;
            }
            return;
        }
        let success = result.is_ok();
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.signing_in = false;
            if let Some(attempt) = state.attempt.take() {
                if state.cancelled_attempts.len() >= 32 {
                    state.cancelled_attempts.pop_front();
                }
                state.cancelled_attempts.push_back(attempt);
            }
            if let (Some(tokens), Ok(())) = (&tokens, &result) {
                state.identity = Some(tokens.identity().clone());
                state.pending_registration = None;
                state.tokens = Some(tokens.clone());
                state.pending_rotation = None;
                state.stopped = false;
                state.session = state.session.saturating_add(1);
                state.models.clear();
                state.fault = None;
                self.cancellation
                    .send_modify(|epoch| *epoch = epoch.saturating_add(1));
            } else {
                state.fault = result.err();
            }
        }
        if !success {
            if let Some(tokens) = tokens {
                self.retire_issued(tokens).await;
            }
        }
        drop(guard);
        (self.changed)();
        if success {
            let _ = self.refresh_models().await;
        }
    }

    // Called under the mutation lock. Revocation can invalidate related grant
    // tokens; never risk revoking a previously active session merely because a
    // replacement sign-in was cancelled. Keep that cleanup for explicit logout.
    async fn retire_issued(&self, tokens: Arc<Tokens>) {
        let previous_active = {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            state.tokens.is_some() || state.pending_rotation.is_some()
        };
        let revoked = !previous_active
            && tokio::time::timeout(Duration::from_secs(20), self.transport.revoke(&tokens))
                .await
                .is_ok_and(|result| result.is_ok());
        if !revoked {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.pending_revocations.len() < 4 {
                state.pending_revocations.push(tokens);
            } else {
                state.revocation_lost = true;
            }
            state.revocation_unconfirmed = true;
        }
        (self.changed)();
    }

    /// Cancel only pending authorization. A previously connected account stays intact.
    pub async fn cancel(&self, attempt: &str) {
        if !valid_attempt(attempt) {
            return;
        }
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if !state.cancelled_attempts.iter().any(|id| id == attempt) {
                if state.cancelled_attempts.len() >= 32 {
                    state.cancelled_attempts.pop_front();
                }
                state.cancelled_attempts.push_back(attempt.into());
            }
            if !state.signing_in || state.attempt.as_deref() != Some(attempt) {
                return;
            }
            state.attempt = None;
            state.generation = state.generation.saturating_add(1);
            state.signing_in = false;
            state.fault = Some(Fault::Cancelled);
            self.cancellation
                .send_modify(|epoch| *epoch = epoch.saturating_add(1));
        }
        (self.changed)();
        // Any in-progress vault commit must finish its stale-generation rollback
        // before the caller sees this cancellation as complete.
        let _guard = self.mutation.lock().await;
    }

    /// Follow the authoritative native profile projection. Backgrounding a
    /// profile retires pending OAuth and old Work leases without deleting its
    /// durable account. It does not depend on renderer disposal/cancel IPC.
    pub fn set_foreground(&self, foreground: bool) {
        if foreground
            && self
                .tasks
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .closed
        {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if state.foreground == foreground {
            return;
        }
        state.foreground = foreground;
        if !foreground {
            state.generation = state.generation.saturating_add(1);
            state.session = state.session.saturating_add(1);
            state.signing_in = false;
            if let Some(attempt) = state.attempt.take() {
                if state.cancelled_attempts.len() >= 32 {
                    state.cancelled_attempts.pop_front();
                }
                state.cancelled_attempts.push_back(attempt);
            }
            self.cancellation
                .send_modify(|epoch| *epoch = epoch.saturating_add(1));
        }
        drop(state);
    }

    /// Immediately retire network output and pending authorization during app
    /// shutdown. This does not revoke or delete the persisted account.
    pub fn stop(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        state.session = state.session.saturating_add(1);
        state.generation = state.generation.saturating_add(1);
        state.signing_in = false;
        state.attempt = None;
        state.models.clear();
        self.cancellation
            .send_modify(|epoch| *epoch = epoch.saturating_add(1));
        drop(state);
        (self.changed)();
    }

    /// Obtain a current credential, committing a complete refresh before use.
    async fn credential(self: &Arc<Self>) -> Result<Arc<Tokens>, Fault> {
        let service = self.clone();
        self.spawn(async move { service.credential_owned().await })?
            .await
            .map_err(|_| Fault::Vault)?
    }

    async fn credential_owned(&self) -> Result<Arc<Tokens>, Fault> {
        self.load_owned().await?;
        let _guard = self.mutation.lock().await;
        let pending = self
            .state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .pending_rotation
            .clone();
        if let Some(pending) = pending {
            // Retry only the protected write, not the consumed refresh grant.
            self.persist_rotation(pending).await?;
        }
        let tokens = {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            if state.stopped || !state.foreground {
                return Err(Fault::SessionExpired);
            }
            state.tokens.clone().ok_or(Fault::SessionExpired)?
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Fault::Invalid)?
            .as_secs();
        if tokens.expires_at() > now.saturating_add(60) {
            return Ok(tokens);
        }
        // Do not drop a one-use refresh after it was sent. Its bounded result
        // must be saved for the queued sign-out to revoke, but cannot authorize
        // more inference after the synchronous stop fence.
        let fresh = match self.transport.refresh(&tokens).await {
            Ok(fresh) => fresh,
            Err(error) => {
                let fault = Fault::from(error);
                if fault == Fault::SessionExpired {
                    let saved = self.save(Some(tokens.identity().clone()), None, None).await;
                    let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                    state.tokens = None;
                    state.session = state.session.saturating_add(1);
                    state.models.clear();
                    state.fault = Some(if saved.is_err() { Fault::Vault } else { fault });
                    self.cancellation
                        .send_modify(|epoch| *epoch = epoch.saturating_add(1));
                    drop(state);
                    (self.changed)();
                } else if !self.state.read().unwrap_or_else(|e| e.into_inner()).stopped {
                    self.failed(fault);
                }
                return Err(fault);
            }
        };
        let fresh = Arc::new(fresh);
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.pending_rotation = Some(fresh.clone());
            state.tokens = None;
        }
        self.persist_rotation(fresh.clone()).await?;
        let stopped = {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            state.stopped || !state.foreground
        };
        if stopped {
            Err(Fault::Cancelled)
        } else {
            Ok(fresh)
        }
    }

    // Caller owns the mutation lock. A verified rotation survives a failed
    // protected write only in native memory; no inference until durability.
    async fn persist_rotation(&self, fresh: Arc<Tokens>) -> Result<(), Fault> {
        if self
            .save(Some(fresh.identity().clone()), Some(&fresh), None)
            .await
            .is_err()
        {
            self.failed(Fault::Vault);
            return Err(Fault::Vault);
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.tokens = Some(fresh);
        state.pending_rotation = None;
        state.fault = None;
        drop(state);
        (self.changed)();
        Ok(())
    }

    /// Fetch this account's catalog, not proof of inference entitlement.
    pub async fn refresh_models(self: &Arc<Self>) -> Result<(), Fault> {
        let mut cancellation = self.cancellation.subscribe();
        let tokens = self.credential().await?;
        if cancellation.has_changed().unwrap_or(true) {
            return Err(Fault::Cancelled);
        }
        let result = tokio::select! {
            biased;
            _ = cancellation.changed() => return Err(Fault::Cancelled),
            result = self.transport.models(&tokens) => result.map_err(model_fault),
        };
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.stopped
                || !state.foreground
                || cancellation.has_changed().unwrap_or(true)
                || !state
                    .tokens
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &tokens))
            {
                return Err(Fault::Cancelled);
            }
            match result.as_ref() {
                Ok(models) => {
                    state.models = models.clone();
                    state.fault = None;
                }
                Err(fault) => state.fault = Some(*fault),
            }
        }
        (self.changed)();
        result.map(|_| ())
    }

    /// Capture the current grant for a native Work client, without credentials.
    pub fn lease(&self) -> Option<Lease> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        (!state.stopped
            && state.foreground
            && state.pending_rotation.is_none()
            && state.tokens.is_some())
        .then_some(Lease(state.session))
    }

    fn accepts(&self, lease: Lease, model: &str) -> bool {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        !state.stopped
            && state.foreground
            && state.pending_rotation.is_none()
            && state.tokens.is_some()
            && state.session == lease.0
            && state.models.iter().any(|entry| entry.slug == model)
    }

    /// Make a caller-authorized Work request. Sign-out fences pending output.
    pub async fn call(
        self: &Arc<Self>,
        lease: Lease,
        request: WorkModelRequest,
        events: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<WorkModelOutcome, WorkModelError> {
        let mut cancellation = self.cancellation.subscribe();
        if !self.accepts(lease, &request.model.model) {
            return Err(work_fault(Fault::SessionExpired));
        }
        let tokens = self.credential().await.map_err(work_fault)?;
        if !self.accepts(lease, &request.model.model) {
            return Err(work_fault(Fault::SessionExpired));
        }
        if cancellation.has_changed().unwrap_or(true) {
            return Err(work_fault(Fault::Cancelled));
        }
        let fence = cancellation.clone();
        let fenced_events = |event| {
            if !fence.has_changed().unwrap_or(true) {
                events(event);
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancellation.changed() => Err(work_fault(Fault::Cancelled)),
            result = self.transport.call(&tokens, request, &fenced_events) => result,
        };
        if fence.has_changed().unwrap_or(true) {
            Err(work_fault(Fault::Cancelled))
        } else {
            result
        }
    }

    /// Stop requests, attempt revocation, then clear local tokens but retain mapping.
    pub async fn sign_out(self: &Arc<Self>) -> Result<(), Fault> {
        // Publish before waiting on a refresh/vault lock: even a simultaneous
        // ready stream terminal cannot deliver after this operation is observed.
        self.stop();
        let service = self.clone();
        self.spawn(async move { service.sign_out_owned().await })?
            .await
            .map_err(|_| Fault::Vault)?
    }

    async fn sign_out_owned(&self) -> Result<(), Fault> {
        self.load_owned().await?;
        let _guard = self.mutation.lock().await;
        let (identity, pending, revocations) = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.fault = None;
            let current = state
                .pending_rotation
                .take()
                .or_else(|| state.tokens.take());
            state.tokens = None;
            if let Some(tokens) = current {
                if state.pending_revocations.len() < 4 {
                    state.pending_revocations.push(tokens);
                } else {
                    state.revocation_lost = true;
                }
            }
            (
                state.identity.clone(),
                state.pending_registration.clone(),
                state.pending_revocations.clone(),
            )
        };
        // Local deletion does not wait for an unavailable revocation server.
        let saved = if identity.is_some() || pending.is_some() {
            self.save(identity, None, pending).await
        } else {
            Ok(())
        };
        let mut revoke = tokio::task::JoinSet::new();
        for tokens in revocations {
            let transport = self.transport.clone();
            revoke.spawn(async move {
                let success =
                    tokio::time::timeout(Duration::from_secs(20), transport.revoke(&tokens))
                        .await
                        .is_ok_and(|result| result.is_ok());
                (tokens, success)
            });
        }
        while let Some(result) = revoke.join_next().await {
            if let Ok((tokens, true)) = result {
                self.state
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending_revocations
                    .retain(|pending| !Arc::ptr_eq(pending, &tokens));
            }
        }
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.revocation_unconfirmed =
                state.revocation_lost || !state.pending_revocations.is_empty();
            state.fault = saved.as_ref().err().copied();
        }
        (self.changed)();
        saved
    }
}

/// Validate a UI operation identity without granting any account authority.
pub fn valid_attempt(attempt: &str) -> bool {
    attempt.len() == 36
        && attempt.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            14 => b == b'4',
            19 => matches!(b, b'8' | b'9' | b'a' | b'b'),
            _ => b.is_ascii_hexdigit() && !b.is_ascii_uppercase(),
        })
}

fn work_fault(fault: Fault) -> WorkModelError {
    WorkModelError::ChatGpt {
        fault: match fault {
            Fault::SessionExpired | Fault::Cancelled => ChatGptModelFault::SignedOut,
            Fault::UsageLimit => ChatGptModelFault::UsageLimit,
            Fault::Denied => ChatGptModelFault::NotEligible,
            Fault::Invalid | Fault::Protocol => ChatGptModelFault::Unsupported,
            _ => ChatGptModelFault::Unavailable,
        },
    }
}

fn model_fault(error: WorkModelError) -> Fault {
    match error {
        WorkModelError::ChatGpt { fault } => match fault {
            ChatGptModelFault::SignedOut => Fault::SessionExpired,
            ChatGptModelFault::UsageLimit => Fault::UsageLimit,
            ChatGptModelFault::Unavailable => Fault::Offline,
            ChatGptModelFault::NotEligible => Fault::Denied,
            ChatGptModelFault::Unsupported => Fault::Protocol,
        },
        WorkModelError::Unauthorized | WorkModelError::MissingKey => Fault::SessionExpired,
        WorkModelError::RateLimited { .. }
        | WorkModelError::OverBudget
        | WorkModelError::PlanLimit => Fault::UsageLimit,
        _ => Fault::Protocol,
    }
}

async fn callback(
    listener: &TcpListener,
    attempt: &LoginAttempt,
) -> Result<Zeroizing<String>, Fault> {
    for _ in 0..64 {
        let (mut socket, peer) = listener.accept().await.map_err(|_| Fault::Offline)?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let mut request = Zeroizing::new(Vec::new());
        let read = tokio::time::timeout(Duration::from_secs(2), async {
            let mut part = Zeroizing::new([0_u8; 512]);
            while request.len() < 8192 {
                let count = socket.read(&mut part[..]).await.ok()?;
                if count == 0 {
                    return None;
                }
                if request.len().saturating_add(count) > 8192 {
                    return None;
                }
                request.extend_from_slice(&part[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    return Some(());
                }
            }
            None
        })
        .await;
        let target = if matches!(read, Ok(Some(()))) {
            std::str::from_utf8(&request).ok().and_then(|request| {
                let mut words = request.lines().next()?.split(' ');
                if words.next()? != "GET" {
                    return None;
                }
                let target = words.next()?;
                if words.next()? != "HTTP/1.1"
                    || words.next().is_some()
                    || !target.starts_with("/auth/callback?")
                {
                    return None;
                }
                Some(target)
            })
        } else {
            None
        };
        let url = target.map(|target| {
            Zeroizing::new(format!(
                "http://127.0.0.1:{}{target}",
                listener.local_addr().map(|a| a.port()).unwrap_or(0)
            ))
        });
        let matched = url
            .as_ref()
            .is_some_and(|url| attempt.matches_callback(url));
        let status = if matched { "200 OK" } else { "400 Bad Request" };
        let body = if matched {
            "Return to Zephium to check the sign-in result. You may close this tab."
        } else {
            "This callback does not match the pending sign-in."
        };
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            socket.write_all(response.as_bytes()),
        )
        .await;
        if matched {
            return url.ok_or(Fault::Protocol);
        }
    }
    Err(Fault::Protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Record;
    use crate::vault::SaveOutcome;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex as StdMutex};
    use tokio::sync::Notify;

    const PROFILE: &str = "00000000000000000000000001";
    const HOST: &str = "urn:uuid:12345678-1234-4123-8123-123456789abc";
    const ATTEMPT: &str = "12345678-1234-4123-8123-123456789abc";
    const OTHER: &str = "12345678-1234-4123-8123-123456789abd";

    fn token(label: &str, expires_at: u64) -> Tokens {
        Tokens(Record {
            version: 1,
            identity: Identity {
                issuer: "https://auth.openai.com".into(),
                subject: "synthetic-user".into(),
                client_id: "oaiapp_synthetic".into(),
                host_id: HOST.into(),
                email: None,
            },
            access_token: label.to_owned().into(),
            refresh_token: format!("refresh-{label}").into(),
            id_token: "synthetic".to_owned().into(),
            nonce: "synthetic-nonce".to_owned().into(),
            scopes: vec![
                "openid".into(),
                "resource.invoke".into(),
                "chatgpt.tokens.use.direct".into(),
            ],
            expires_at,
        })
    }
    fn copy(record: &Stored) -> Stored {
        Stored {
            identity: record.identity.clone(),
            pending_registration: record.pending_registration.clone(),
            tokens: record.tokens.clone(),
        }
    }
    #[derive(Default)]
    struct SaveGate {
        entered: Notify,
        ready: StdMutex<bool>,
        released: Condvar,
    }
    impl SaveGate {
        fn wait(&self) {
            self.entered.notify_one();
            let mut ready = self.ready.lock().unwrap();
            while !*ready {
                ready = self.released.wait(ready).unwrap();
            }
        }
        fn release(&self) {
            *self.ready.lock().unwrap() = true;
            self.released.notify_all();
        }
    }
    #[derive(Default)]
    struct MemoryVault {
        record: StdMutex<Option<Stored>>,
        gate: StdMutex<Option<Arc<SaveGate>>>,
        cleanup: AtomicBool,
        fail_save: AtomicBool,
    }
    impl AccountVault for MemoryVault {
        fn forget(&self, _: &str) -> Result<(), VaultError> {
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(VaultError::Inaccessible);
            }
            *self.record.lock().unwrap() = None;
            Ok(())
        }
        fn load(&self, _: &str) -> Result<Stored, VaultError> {
            self.record
                .lock()
                .unwrap()
                .as_ref()
                .map(copy)
                .ok_or(VaultError::Missing)
        }
        fn save(&self, _: &str, record: &Stored) -> Result<SaveOutcome, VaultError> {
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(VaultError::Inaccessible);
            }
            let gate = self.gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.wait();
            }
            *self.record.lock().unwrap() = Some(copy(record));
            Ok(SaveOutcome {
                cleanup_pending: self.cleanup.load(Ordering::SeqCst),
            })
        }
    }
    #[derive(Default)]
    struct Fake {
        refreshes: AtomicUsize,
        refresh_started: Notify,
        refresh_release: Notify,
        block_refresh: AtomicBool,
        models_started: Notify,
        models_release: Notify,
        block_models: AtomicBool,
        revoked: StdMutex<Vec<String>>,
        revoke_fails: AtomicBool,
    }
    impl Transport for Fake {
        fn exchange(&self, _: Authorization) -> PortFuture<'_, Result<Tokens, AuthError>> {
            Box::pin(async { panic!("test must never exchange a real authorization") })
        }
        fn refresh<'a>(&'a self, _: &'a Tokens) -> PortFuture<'a, Result<Tokens, AuthError>> {
            Box::pin(async move {
                self.refreshes.fetch_add(1, Ordering::SeqCst);
                self.refresh_started.notify_one();
                if self.block_refresh.load(Ordering::SeqCst) {
                    self.refresh_release.notified().await;
                }
                Ok(token("rotated", u64::MAX))
            })
        }
        fn revoke<'a>(&'a self, tokens: &'a Tokens) -> PortFuture<'a, Result<(), AuthError>> {
            Box::pin(async move {
                self.revoked
                    .lock()
                    .unwrap()
                    .push(tokens.access_token().into());
                if self.revoke_fails.load(Ordering::SeqCst) {
                    Err(AuthError::Transport)
                } else {
                    Ok(())
                }
            })
        }
        fn models<'a>(
            &'a self,
            _: &'a Tokens,
        ) -> PortFuture<'a, Result<Vec<Model>, WorkModelError>> {
            Box::pin(async move {
                self.models_started.notify_one();
                if self.block_models.load(Ordering::SeqCst) {
                    self.models_release.notified().await;
                    return Err(work_fault(Fault::UsageLimit));
                }
                Ok(Vec::new())
            })
        }
        fn call<'a>(
            &'a self,
            _: &'a Tokens,
            _: WorkModelRequest,
            _: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
        ) -> PortFuture<'a, Result<WorkModelOutcome, WorkModelError>> {
            Box::pin(async { panic!("no inference request expected") })
        }
    }
    fn fixture(tokens: Option<Tokens>) -> (Arc<Service>, Arc<MemoryVault>, Arc<Fake>) {
        let vault = Arc::new(MemoryVault::default());
        let transport = Arc::new(Fake::default());
        let (cancellation, _) = watch::channel(0);
        let identity = tokens.as_ref().map(|tokens| tokens.identity().clone());
        let service = Arc::new(Service {
            profile: PROFILE.into(),
            host_id: HOST.into(),
            transport: transport.clone(),
            vault: vault.clone(),
            mutation: Mutex::new(()),
            state: RwLock::new(State {
                loaded: true,
                foreground: true,
                identity,
                tokens: tokens.map(Arc::new),
                ..Default::default()
            }),
            cancellation,
            changed: Arc::new(|| {}),
            tasks: Arc::new(Tasks::default()),
        });
        (service, vault, transport)
    }

    #[tokio::test]
    async fn cancel_before_start_prevents_browser_and_foreign_cancel_does_not_retire_owner() {
        let (service, _, _) = fixture(None);
        service.cancel(ATTEMPT).await;
        assert_eq!(
            service
                .sign_in(ATTEMPT.into(), |_| async {
                    panic!("cancelled attempt must not open browser")
                })
                .await,
            Err(Fault::Cancelled)
        );
        {
            let mut state = service.state.write().unwrap();
            state.signing_in = true;
            state.attempt = Some(OTHER.into());
            state.generation = 4;
        }
        service.cancel(ATTEMPT).await;
        assert_eq!(service.view().login_attempt.as_deref(), Some(OTHER));
        assert_eq!(service.state.read().unwrap().generation, 4);
    }

    #[tokio::test]
    async fn stale_login_result_cannot_replace_newer_operation() {
        let (service, vault, _) = fixture(None);
        {
            let mut state = service.state.write().unwrap();
            state.generation = 2;
            state.signing_in = true;
            state.attempt = Some(OTHER.into());
        }
        service.finish_login(1, Ok(token("stale", u64::MAX))).await;
        assert!(!service.view().connected);
        assert_eq!(service.view().login_attempt.as_deref(), Some(OTHER));
        assert!(vault.record.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn cancel_during_vault_commit_restores_previous_session_before_acknowledging() {
        let (service, vault, _) = fixture(Some(token("previous", u64::MAX)));
        {
            let mut state = service.state.write().unwrap();
            state.generation = 1;
            state.signing_in = true;
            state.attempt = Some(ATTEMPT.into());
        }
        let gate = Arc::new(SaveGate::default());
        *vault.gate.lock().unwrap() = Some(gate.clone());
        let finish = {
            let service = service.clone();
            tokio::spawn(async move {
                service.finish_login(1, Ok(token("new", u64::MAX))).await;
            })
        };
        gate.entered.notified().await;
        let cancel = {
            let service = service.clone();
            tokio::spawn(async move {
                service.cancel(ATTEMPT).await;
            })
        };
        tokio::task::yield_now().await;
        assert!(!service.view().signing_in);
        assert!(!cancel.is_finished());
        gate.release();
        finish.await.unwrap();
        cancel.await.unwrap();
        assert_eq!(
            service.credential().await.unwrap().access_token(),
            "previous"
        );
        let stored = vault.load(PROFILE).unwrap();
        assert_eq!(
            Tokens::from_bytes(&stored.tokens.unwrap())
                .unwrap()
                .access_token(),
            "previous"
        );
    }

    #[tokio::test]
    async fn concurrent_refresh_is_single_flight_and_cleanup_failure_adopts_committed_rotation() {
        let (service, vault, transport) = fixture(Some(token("expired", 1)));
        vault.cleanup.store(true, Ordering::SeqCst);
        transport.block_refresh.store(true, Ordering::SeqCst);
        let one = {
            let service = service.clone();
            tokio::spawn(async move { service.credential().await })
        };
        transport.refresh_started.notified().await;
        let two = {
            let service = service.clone();
            tokio::spawn(async move { service.credential().await })
        };
        transport.refresh_release.notify_one();
        assert_eq!(one.await.unwrap().unwrap().access_token(), "rotated");
        assert_eq!(two.await.unwrap().unwrap().access_token(), "rotated");
        assert_eq!(transport.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(service.view().fault, Some(Fault::Vault));
        assert!(service.view().connected);
    }

    #[tokio::test]
    async fn signout_fences_refresh_immediately_and_revokes_the_rotated_credential() {
        let (service, vault, transport) = fixture(Some(token("expired", 1)));
        transport.block_refresh.store(true, Ordering::SeqCst);
        let refresh = {
            let service = service.clone();
            tokio::spawn(async move { service.credential().await })
        };
        transport.refresh_started.notified().await;
        let signout = {
            let service = service.clone();
            tokio::spawn(async move { service.sign_out().await })
        };
        tokio::task::yield_now().await;
        assert!(!service.view().connected);
        assert!(!signout.is_finished());
        transport.refresh_release.notify_one();
        assert!(matches!(refresh.await.unwrap(), Err(Fault::Cancelled)));
        assert_eq!(signout.await.unwrap(), Ok(()));
        assert_eq!(&*transport.revoked.lock().unwrap(), &["rotated"]);
        assert!(vault.load(PROFILE).unwrap().tokens.is_none());
    }

    #[tokio::test]
    async fn stale_model_failure_after_stop_cannot_overwrite_current_state() {
        let (service, _, transport) = fixture(Some(token("active", u64::MAX)));
        transport.block_models.store(true, Ordering::SeqCst);
        let models = {
            let service = service.clone();
            tokio::spawn(async move { service.refresh_models().await })
        };
        transport.models_started.notified().await;
        service.stop();
        transport.models_release.notify_one();
        assert_eq!(models.await.unwrap(), Err(Fault::Cancelled));
        assert_eq!(service.view().fault, None);
        assert!(service.models().is_empty());
    }

    #[tokio::test]
    async fn remote_revocation_failure_never_restores_local_tokens() {
        let (service, vault, transport) = fixture(Some(token("active", u64::MAX)));
        transport.revoke_fails.store(true, Ordering::SeqCst);
        assert_eq!(service.sign_out().await, Ok(()));
        assert!(service.view().revocation_unconfirmed);
        assert!(!service.view().connected);
        assert!(vault.load(PROFILE).unwrap().tokens.is_none());
    }

    #[tokio::test]
    async fn unverified_registration_is_retained_for_retry_but_is_not_connected() {
        let (service, vault, _) = fixture(None);
        service
            .remember_registration(0, "oaiapp_synthetic")
            .await
            .unwrap();
        let record = vault.load(PROFILE).unwrap();
        assert!(record.identity.is_none());
        assert!(record.tokens.is_none());
        assert_eq!(
            record.pending_registration.unwrap().client_id,
            "oaiapp_synthetic"
        );
        assert!(!service.view().connected);
    }

    #[test]
    fn operation_ids_and_safe_faults_do_not_accept_provider_secrets() {
        assert!(valid_attempt(ATTEMPT));
        assert!(!valid_attempt("../profile"));
        assert!(!valid_attempt("12345678-1234-1123-8123-123456789abc"));
        assert!(matches!(
            work_fault(Fault::Vault),
            WorkModelError::ChatGpt {
                fault: ChatGptModelFault::Unavailable
            }
        ));
        assert_eq!(
            model_fault(work_fault(Fault::UsageLimit)),
            Fault::UsageLimit
        );
    }
    #[tokio::test]
    async fn dropping_refresh_waiter_does_not_abandon_one_use_rotation() {
        let (service, vault, transport) = fixture(Some(token("expired", 1)));
        transport.block_refresh.store(true, Ordering::SeqCst);
        let waiter = {
            let service = service.clone();
            tokio::spawn(async move { service.credential().await })
        };
        transport.refresh_started.notified().await;
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(service.tasks.state.lock().unwrap().active, 1);
        transport.refresh_release.notify_one();
        service.drained().await;
        assert_eq!(
            service.credential().await.unwrap().access_token(),
            "rotated"
        );
        let stored = vault.load(PROFILE).unwrap();
        assert_eq!(
            Tokens::from_bytes(&stored.tokens.unwrap())
                .unwrap()
                .access_token(),
            "rotated"
        );
        assert_eq!(transport.refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn old_idle_client_cannot_cross_signout_and_reauthorization() {
        let (service, _, _) = fixture(Some(token("first", u64::MAX)));
        service.state.write().unwrap().models = vec![Model {
            slug: "gpt-fixture".into(),
            display_name: "Fixture".into(),
        }];
        let old = service.lease().unwrap();
        assert!(service.accepts(old, "gpt-fixture"));
        service.sign_out().await.unwrap();
        let generation = service.state.read().unwrap().generation;
        service
            .finish_login(generation, Ok(token("second", u64::MAX)))
            .await;
        service.state.write().unwrap().models = vec![Model {
            slug: "gpt-fixture".into(),
            display_name: "Fixture".into(),
        }];
        assert!(!service.accepts(old, "gpt-fixture"));
        assert!(service.accepts(service.lease().unwrap(), "gpt-fixture"));
        assert!(!service.accepts(service.lease().unwrap(), "unknown-model"));
    }

    #[tokio::test]
    async fn close_blocks_admission_and_drain_waits_for_owned_commit() {
        let (service, _, transport) = fixture(Some(token("expired", 1)));
        transport.block_refresh.store(true, Ordering::SeqCst);
        let waiter = {
            let service = service.clone();
            tokio::spawn(async move { service.credential().await })
        };
        transport.refresh_started.notified().await;
        service.close();
        assert_eq!(service.load().await, Err(Fault::Cancelled));
        assert!(matches!(service.credential().await, Err(Fault::Cancelled)));
        let drained = {
            let service = service.clone();
            tokio::spawn(async move { service.drained().await })
        };
        tokio::task::yield_now().await;
        assert!(!drained.is_finished());
        transport.refresh_release.notify_one();
        assert!(matches!(waiter.await.unwrap(), Err(Fault::Cancelled)));
        drained.await.unwrap();
        assert!(!service.view().connected);
    }

    #[tokio::test]
    async fn loopback_callback_rejects_unbound_state_and_consumes_only_matching_get() {
        use tokio::net::TcpStream;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let attempt = LoginAttempt::new(HOST, None, address.port()).unwrap();
        let authorization = url::Url::parse(&attempt.authorization_url().unwrap()).unwrap();
        let state = authorization
            .query_pairs()
            .find(|(name, _)| name == "state")
            .unwrap()
            .1
            .into_owned();
        let accepted = tokio::spawn(async move { callback(&listener, &attempt).await });
        for (state, expected) in [("wrong", "400 Bad Request"), (state.as_str(), "200 OK")] {
            let mut socket = TcpStream::connect(address).await.unwrap();
            let request = format!("GET /auth/callback?state={state}&code=synthetic-code&client_id=oaiapp_synthetic HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", address.port());
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            let response = String::from_utf8(response).unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {expected}")));
            assert!(!response.contains("synthetic-code"));
            assert!(!response.contains("oaiapp_synthetic"));
        }
        assert!(accepted.await.unwrap().unwrap().contains("state="));
    }
    #[tokio::test]
    async fn failed_rotation_write_retains_new_tokens_and_retries_only_persistence() {
        let (service, vault, transport) = fixture(Some(token("expired", 1)));
        vault.fail_save.store(true, Ordering::SeqCst);
        assert!(matches!(service.credential().await, Err(Fault::Vault)));
        assert!(!service.view().connected);
        assert!(service.lease().is_none());
        assert_eq!(
            service
                .state
                .read()
                .unwrap()
                .pending_rotation
                .as_ref()
                .unwrap()
                .access_token(),
            "rotated"
        );
        assert!(matches!(service.credential().await, Err(Fault::Vault)));
        assert_eq!(transport.refreshes.load(Ordering::SeqCst), 1);
        vault.fail_save.store(false, Ordering::SeqCst);
        assert_eq!(
            service.credential().await.unwrap().access_token(),
            "rotated"
        );
        assert_eq!(transport.refreshes.load(Ordering::SeqCst), 1);
        assert!(service.view().connected);
    }

    #[tokio::test]
    async fn repeated_signout_does_not_clear_unconfirmed_revocation_without_success() {
        let (service, vault, transport) = fixture(Some(token("active", u64::MAX)));
        transport.revoke_fails.store(true, Ordering::SeqCst);
        vault.fail_save.store(true, Ordering::SeqCst);
        assert_eq!(service.sign_out().await, Err(Fault::Vault));
        assert!(service.view().revocation_unconfirmed);
        vault.fail_save.store(false, Ordering::SeqCst);
        service.sign_out().await.unwrap();
        assert!(service.view().revocation_unconfirmed);
        assert_eq!(transport.revoked.lock().unwrap().len(), 2);
        transport.revoke_fails.store(false, Ordering::SeqCst);
        service.sign_out().await.unwrap();
        assert!(!service.view().revocation_unconfirmed);
        assert_eq!(transport.revoked.lock().unwrap().len(), 3);
        assert!(vault.load(PROFILE).unwrap().tokens.is_none());
    }
    #[tokio::test]
    async fn native_profile_change_retires_login_and_leases_without_renderer_cancel() {
        let (service, vault, _) = fixture(Some(token("previous", u64::MAX)));
        let lease = service.lease().unwrap();
        {
            let mut state = service.state.write().unwrap();
            state.generation = 1;
            state.signing_in = true;
            state.attempt = Some(ATTEMPT.into());
        }
        service.set_foreground(false);
        assert!(!service.view().signing_in);
        assert!(service.lease().is_none());
        service.finish_login(1, Ok(token("late", u64::MAX))).await;
        assert!(vault.record.lock().unwrap().is_none());
        assert_eq!(
            service
                .sign_in(OTHER.into(), |_| async {
                    panic!("background profile must not open browser")
                })
                .await,
            Err(Fault::Cancelled)
        );
        service.set_foreground(true);
        assert_ne!(service.lease().unwrap(), lease);
        assert_eq!(
            service.credential().await.unwrap().access_token(),
            "previous"
        );
    }
    #[tokio::test]
    async fn closed_profile_rejects_signin_already_queued_behind_mutation() {
        let (service, _, _) = fixture(None);
        let guard = service.mutation.lock().await;
        let signin = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .sign_in(ATTEMPT.into(), |_| async {
                        panic!("retired queued login must not open browser")
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        service.close();
        drop(guard);
        assert_eq!(signin.await.unwrap(), Err(Fault::Cancelled));
        service.drained().await;
        assert!(!service.view().signing_in);
        assert!(service.view().login_attempt.is_none());
    }
    #[tokio::test]
    async fn stale_issued_grant_is_revoked_or_retained_without_touching_previous_session() {
        let (service, _, transport) = fixture(None);
        service.state.write().unwrap().generation = 2;
        transport.revoke_fails.store(true, Ordering::SeqCst);
        service.finish_login(1, Ok(token("stale", u64::MAX))).await;
        assert_eq!(&*transport.revoked.lock().unwrap(), &["stale"]);
        assert!(service.view().revocation_unconfirmed);
        transport.revoke_fails.store(false, Ordering::SeqCst);
        service.sign_out().await.unwrap();
        assert!(!service.view().revocation_unconfirmed);
        let (service, _, transport) = fixture(Some(token("previous", u64::MAX)));
        service.state.write().unwrap().generation = 2;
        service
            .finish_login(1, Ok(token("replacement", u64::MAX)))
            .await;
        assert!(transport.revoked.lock().unwrap().is_empty());
        assert!(service.view().revocation_unconfirmed);
        assert_eq!(
            service.credential().await.unwrap().access_token(),
            "previous"
        );
    }
    #[tokio::test]
    async fn queued_signin_does_not_survive_a_native_profile_scope_bounce() {
        let (service, _, _) = fixture(None);
        let guard = service.mutation.lock().await;
        let signin = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .sign_in(ATTEMPT.into(), |_| async {
                        panic!("old admitted scope must not regain authorization")
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        service.set_foreground(false);
        service.set_foreground(true);
        drop(guard);
        assert_eq!(signin.await.unwrap(), Err(Fault::Cancelled));
        service.drained().await;
        assert!(!service.view().signing_in);
    }
    #[tokio::test]
    async fn profile_erasure_waits_for_rotation_then_removes_all_local_state() {
        let (service, vault, transport) = fixture(Some(token("expired", 1)));
        transport.block_refresh.store(true, Ordering::SeqCst);
        let refresh = {
            let service = service.clone();
            tokio::spawn(async move { service.credential().await })
        };
        transport.refresh_started.notified().await;
        let erase = {
            let service = service.clone();
            tokio::spawn(async move { service.erase().await })
        };
        tokio::task::yield_now().await;
        assert!(!erase.is_finished());
        transport.refresh_release.notify_one();
        assert!(matches!(refresh.await.unwrap(), Err(Fault::Cancelled)));
        erase.await.unwrap().unwrap();
        service.drained().await;
        assert!(matches!(vault.load(PROFILE), Err(VaultError::Missing)));
        assert!(service.state.read().unwrap().identity.is_none());
        assert!(service.state.read().unwrap().pending_rotation.is_none());
        assert!(service.lease().is_none());
    }
}
