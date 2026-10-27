//! Read-only recovery, explicit revocation and erasure of legacy account records.
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;

use tokio::sync::{Mutex, Notify};

use crate::oauth::{AuthError, Client};
use crate::token::{Identity, Tokens};
use crate::vault::{AccountVault, PendingRegistration, Stored, VaultError};

mod recovery;
pub use recovery::{existing_host, unbound_presence};

/// Closed, non-secret account faults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// Invalid configuration or profile identity.
    Invalid,
    /// The provider could not be reached.
    Offline,
    /// Native profile scope or task ownership was retired.
    Cancelled,
    /// A bounded operation timed out.
    Timeout,
    /// A stored record or response violated the expected protocol.
    Protocol,
    /// Native protected storage refused the operation.
    Vault,
    /// Another protected operation is pending.
    Busy,
}

impl From<AuthError> for Fault {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Configuration => Self::Invalid,
            AuthError::Transport => Self::Offline,
            AuthError::Timeout => Self::Timeout,
            _ => Self::Protocol,
        }
    }
}

/// Evidence from protected storage, independent of connection status or email.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Presence {
    /// An authoritative vault read confirmed no registration.
    Missing,
    /// A verified identity or pending-registration-only record exists.
    Stored,
    /// No successful storage observation is available.
    #[default]
    Unavailable,
}

/// Non-secret native projection. Connected is not an entitlement guarantee.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// A verified registration has a locally stored renewable session.
    pub connected: bool,
    /// Native evidence of an existing protected registration, even without tokens.
    pub presence: Presence,
    /// Account label from the validated identity token.
    pub email: Option<String>,
    /// Closed result of the latest user operation.
    pub fault: Option<Fault>,
    /// Local sign-out finished without confirming remote revocation.
    pub revocation_unconfirmed: bool,
}

#[derive(Default)]
struct State {
    loaded: bool,
    stopped: bool,
    // An admitted but uncommitted disconnect fences use without deleting a grant.
    pending_signout: Option<u64>,
    foreground: bool,
    identity: Option<Identity>,
    pending_registration: Option<PendingRegistration>,
    cleanup_pending: bool,
    tokens: Option<Arc<Tokens>>,
    pending_revocations: Vec<Arc<Tokens>>,
    revocation_lost: bool,
    generation: u64,
    fault: Option<Fault>,
    revocation_unconfirmed: bool,
}

type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// Fixed production transport; the private seam permits deterministic offline
// lifecycle tests without a configurable token/inference endpoint in the product.
trait Transport: Send + Sync {
    fn revoke<'a>(&'a self, tokens: &'a Tokens) -> PortFuture<'a, Result<(), AuthError>>;
}
struct Official {
    oauth: Client,
}
impl Transport for Official {
    fn revoke<'a>(&'a self, tokens: &'a Tokens) -> PortFuture<'a, Result<(), AuthError>> {
        Box::pin(self.oauth.revoke(tokens))
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

/// Opaque profile admission captured under the native registry's scope lock.
/// Not an IPC value; an away/back transition or account mutation retires it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scope {
    generation: u64,
    profile: zephium_core::ids::ProfileId,
}

/// One browser profile's account. Never shared with another profile or Cloud.
pub struct Service {
    profile: String,
    host_id: String,
    transport: Arc<dyn Transport>,
    vault: Arc<dyn AccountVault>,
    mutation: Mutex<()>,
    state: RwLock<State>,
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
        Ok(Self {
            profile,
            host_id,
            transport: Arc::new(Official {
                oauth: Client::new()?,
            }),
            vault,
            mutation: Mutex::new(()),
            state: RwLock::new(State::default()),
            changed,
            tasks: Arc::new(Tasks::default()),
        })
    }

    // A ticket stays inside the owned task, not the caller's join future.
    // Dropping IPC waiters cannot abandon a protected write or revocation.
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
    /// normal sign-out retains the registration for recovery.
    pub fn close(&self) {
        let first = {
            let mut tasks = self.tasks.state.lock().unwrap_or_else(|e| e.into_inner());
            let first = !tasks.closed;
            tasks.closed = true;
            first
        };
        // A concurrent first close may have latched task admission but not yet
        // retired the foreground lease. Every returning close must fence output.
        self.set_foreground(false);
        if first {
            self.stop();
        }
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
            state.identity = None;
            state.pending_registration = None;
            state.pending_revocations.clear();
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
                && state.pending_signout.is_none()
                && state.foreground
                && state.tokens.is_some(),
            presence: if !state.loaded {
                Presence::Unavailable
            } else if state.identity.is_some() || state.pending_registration.is_some() {
                Presence::Stored
            } else {
                Presence::Missing
            },
            email: state
                .identity
                .as_ref()
                .and_then(|identity| identity.email.clone()),
            fault: state
                .fault
                .or(state.cleanup_pending.then_some(Fault::Vault)),
            revocation_unconfirmed: state.revocation_unconfirmed,
        }
    }

    /// Capture native profile scope without I/O. The host captures this while
    /// verifying its authoritative profile epoch under the same read lock.
    pub fn scope(&self) -> Result<Scope, Fault> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if !state.foreground {
            return Err(Fault::Cancelled);
        }
        Ok(Scope {
            generation: state.generation,
            profile: zephium_core::ids::ProfileId::parse(&self.profile).ok_or(Fault::Invalid)?,
        })
    }
    fn matches_scope(&self, state: &State, scope: Scope) -> bool {
        state.foreground
            && state.generation == scope.generation
            && scope.profile.to_string() == self.profile
    }
    fn require_scope(&self, scope: Scope) -> Result<(), Fault> {
        if self.matches_scope(&self.state.read().unwrap_or_else(|e| e.into_inner()), scope) {
            Ok(())
        } else {
            Err(Fault::Cancelled)
        }
    }
    /// Load only for the atomically captured native scope; never project a stale result.
    pub async fn load_scoped(self: &Arc<Self>, scope: Scope) -> Result<(), Fault> {
        let service = self.clone();
        self.spawn(async move {
            service.require_scope(scope)?;
            service.load_owned().await?;
            service.require_scope(scope)
        })?
        .await
        .map_err(|_| Fault::Vault)?
    }

    /// Load the profile's exact vault record on explicit account observation.
    pub async fn load(self: &Arc<Self>) -> Result<(), Fault> {
        self.load_scoped(self.scope()?).await
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
                state.fault = None;
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
        pending_registration: Option<PendingRegistration>,
    ) -> Result<(), Fault> {
        let record = Stored {
            identity,
            pending_registration,
            tokens: None,
        };
        let profile = self.profile.clone();
        let vault = self.vault.clone();
        let outcome = tokio::task::spawn_blocking(move || vault.save(&profile, &record))
            .await
            .map_err(|_| Fault::Vault)?
            .map_err(|_| Fault::Vault)?;
        // Ok means the new manifest IS committed. Residual old-chunk deletion
        // failure remains an explicit warning after local disconnect commits.
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .cleanup_pending = outcome.cleanup_pending;
        Ok(())
    }

    /// Follow the authoritative native profile projection. Backgrounding a
    /// profile retires outstanding recovery admissions without deleting its
    /// durable account. It does not depend on renderer disposal.
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
            state.pending_signout = None;
            state.generation = state.generation.saturating_add(1);
        }
        drop(state);
    }

    /// Immediately retire network output during app
    /// shutdown. This does not revoke or delete the persisted account.
    pub fn stop(&self) {
        let _ = self.stop_for(None, true);
    }
    fn stop_for(&self, scope: Option<Scope>, stop_account: bool) -> Result<u64, Fault> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if scope.is_some_and(|scope| !self.matches_scope(&state, scope)) {
            return Err(Fault::Cancelled);
        }
        if stop_account {
            state.stopped = true;
        }
        state.generation = state.generation.saturating_add(1);
        state.pending_signout = (!stop_account).then_some(state.generation);
        let generation = state.generation;
        drop(state);
        (self.changed)();
        Ok(generation)
    }

    /// Stop requests, attempt revocation, then clear local tokens but retain mapping.
    pub async fn sign_out(self: &Arc<Self>) -> Result<(), Fault> {
        self.sign_out_scoped(self.scope()?).await
    }

    /// Stop and disconnect only within an atomically captured native profile scope.
    pub async fn sign_out_scoped(self: &Arc<Self>, scope: Scope) -> Result<(), Fault> {
        let generation = self.stop_for(Some(scope), false)?;
        let scope = Scope {
            generation,
            ..scope
        };
        let service = self.clone();
        let task = self.spawn(async move {
            let result = service.sign_out_owned(scope).await;
            if let Err(fault) = result {
                service.abandon_pending_signout(scope.generation, fault);
            }
            result
        });
        match task {
            Ok(task) => task.await.map_err(|_| Fault::Vault)?,
            Err(fault) => {
                self.abandon_pending_signout(scope.generation, fault);
                Err(fault)
            }
        }
    }
    fn abandon_pending_signout(&self, generation: u64, fault: Fault) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if state.pending_signout != Some(generation) {
            return;
        }
        state.pending_signout = None;
        state.fault = Some(fault);
        drop(state);
        (self.changed)();
    }
    async fn sign_out_owned(&self, scope: Scope) -> Result<(), Fault> {
        self.require_scope(scope)?;
        self.load_owned().await?;
        let _guard = self.mutation.lock().await;
        let (identity, pending, revocations) = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if !self.matches_scope(&state, scope) {
                return Err(Fault::Cancelled);
            }
            state.stopped = true;
            state.pending_signout = None;
            state.fault = None;
            let current = state.tokens.take();
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
            self.save(identity, pending).await
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

#[cfg(test)]
mod tests;
