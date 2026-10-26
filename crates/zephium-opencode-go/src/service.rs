//! Owned profile mutations and revocable OpenCode Go clients. No automatic network I/O.
pub mod quota;

use crate::{new_id, valid_profile, vault::Vault, Config};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{watch, Mutex, Notify};
use zephium_agentic::lead::{
    opencode_go::{
        quota::{GoQuotaFault, GoQuotaSnapshot},
        BillingSource, GoFailure, GoFault, GoModel, GoOutcome, OpenCodeGoClient, SessionId,
    },
    LeadSecret,
};
use zephium_core::work::model::*;
use zeroize::Zeroizing;

/// Closed non-secret configuration/observation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// Invalid key or missing explicit billing-control acknowledgement.
    Invalid,
    /// The OpenCode Go could not be reached within its bound.
    Unavailable,
    /// Native storage refused an operation; no successful save is claimed.
    Vault,
    /// There is no usable OpenCode Go configuration.
    NotConfigured,
    /// The connection was replaced after the caller observed it.
    Changed,
    /// Too many owned operations are pending.
    Busy,
    /// This profile/operation no longer owns the result.
    Cancelled,
    /// The OpenCode Go rejected its own key.
    Unauthorized,
    /// The OpenCode Go rejected the request or model.
    Forbidden,
    /// The OpenCode Go reported a rate/usage limit; there is no automatic retry.
    RateLimited,
    /// A bounded OpenCode Go response violated the supported protocol.
    Protocol,
    /// The inference stream never completed successfully.
    Incomplete,
    /// The response omitted accounting required by Work.
    UsageUnavailable,
}
impl From<GoFault> for Fault {
    fn from(f: GoFault) -> Self {
        match f {
            GoFault::InvalidRequest => Self::Invalid,
            GoFault::Unavailable | GoFault::Timeout => Self::Unavailable,
            GoFault::Unauthorized => Self::Unauthorized,
            GoFault::Forbidden => Self::Forbidden,
            GoFault::RateLimited { .. } => Self::RateLimited,
            GoFault::Protocol | GoFault::TooLarge => Self::Protocol,
            GoFault::Incomplete => Self::Incomplete,
            GoFault::UsageUnavailable => Self::UsageUnavailable,
        }
    }
}

/// Safe account-independent view. Configured does not mean reachable or subscribed.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// A configuration was durably saved for this profile.
    pub configured: bool,
    /// Native-generated connection identity for separate model selection.
    pub connection_id: Option<String>,
    /// Number of models from the last successful explicit catalog check.
    pub models: usize,
    /// A user-requested catalog check is pending.
    pub checking: bool,
    /// Unix milliseconds of the last completed catalog attempt, not paid inference.
    pub checked_at_ms: Option<u64>,
    /// Closed fault from the latest explicit observation.
    pub fault: Option<Fault>,
    /// Last completed request attribution; never a remaining quota.
    pub last_billing: Option<BillingObservation>,
    /// Explicit transient provider allowance; separate from billing and catalog.
    pub quota: quota::QuotaView,
}

/// Last completed request's provider-reported billing route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BillingObservation {
    /// Actual response attribution, not a prediction or plan check.
    pub source: BillingSource,
    /// Unix milliseconds observed locally after validated completion.
    pub observed_at_ms: u64,
}

/// Opaque native admission captured while the authoritative profile scope is locked.
/// It is never supplied by IPC and cannot be reused after an away/back transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scope {
    generation: u64,
    profile: zephium_core::ids::ProfileId,
}

/// Revocable, native-only authorization for one exact catalog model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lease {
    generation: u64,
    model_id: String,
    model: String,
    wire: WorkModelWire,
}

#[derive(Default)]
struct State {
    loaded: bool,
    closed: bool,
    foreground: bool,
    generation: u64,
    config: Option<Arc<Config>>,
    models: Vec<GoModel>,
    checking: bool,
    checked_at_ms: Option<u64>,
    fault: Option<Fault>,
    last_billing: Option<BillingObservation>,
    quota: quota::QuotaState,
}
#[derive(Default)]
struct TaskState {
    active: usize,
    closed: bool,
}
#[derive(Default)]
struct Tasks {
    state: StdMutex<TaskState>,
    changed: Notify,
}
struct Guard(Arc<Tasks>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active -= 1;
        self.0.changed.notify_waiters();
    }
}
type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
trait Transport: Send + Sync {
    fn quota<'a>(
        &'a self,
        config: &'a Config,
    ) -> PortFuture<'a, Result<GoQuotaSnapshot, GoQuotaFault>>;
    fn models<'a>(&'a self, config: &'a Config) -> PortFuture<'a, Result<Vec<GoModel>, GoFailure>>;
    fn call<'a>(
        &'a self,
        config: &'a Config,
        session: SessionId,
        request: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> PortFuture<'a, Result<GoOutcome, GoFailure>>;
}
struct Official;
fn client(config: &Config) -> Result<OpenCodeGoClient, GoFailure> {
    let fault = |fault| GoFailure {
        fault,
        dispatched: false,
        output_started: false,
    };
    let key =
        LeadSecret::new(config.key.to_string()).map_err(|_| fault(GoFault::InvalidRequest))?;
    OpenCodeGoClient::new(Arc::new(key), config.id.clone()).map_err(fault)
}
impl Transport for Official {
    fn quota<'a>(
        &'a self,
        config: &'a Config,
    ) -> PortFuture<'a, Result<GoQuotaSnapshot, GoQuotaFault>> {
        Box::pin(async move {
            client(config)
                .map_err(|_| GoQuotaFault::Unavailable)?
                .quota()
                .await
        })
    }
    fn models<'a>(&'a self, config: &'a Config) -> PortFuture<'a, Result<Vec<GoModel>, GoFailure>> {
        Box::pin(async move {
            client(config)?
                .models()
                .await
                .map(|catalog| catalog.supported)
        })
    }
    fn call<'a>(
        &'a self,
        config: &'a Config,
        session: SessionId,
        request: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> PortFuture<'a, Result<GoOutcome, GoFailure>> {
        Box::pin(async move { client(config)?.call(&session, request, events).await })
    }
}

/// One persistent browser profile's separately stored OpenCode Go connection.
pub struct Service {
    profile: String,
    vault: Arc<dyn Vault>,
    transport: Arc<dyn Transport>,
    state: RwLock<State>,
    mutation: Mutex<()>,
    cancellation: watch::Sender<u64>,
    changed: Arc<dyn Fn() + Send + Sync>,
    tasks: Arc<Tasks>,
}
impl Service {
    /// Construct without reading storage, discovering endpoints or sending prompts.
    pub fn new(
        profile: String,
        vault: Arc<dyn Vault>,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, Fault> {
        if !valid_profile(&profile) {
            return Err(Fault::Invalid);
        }
        let (cancellation, _) = watch::channel(0);
        Ok(Self {
            profile,
            vault,
            transport: Arc::new(Official),
            state: RwLock::new(State::default()),
            mutation: Mutex::new(()),
            cancellation,
            changed,
            tasks: Arc::new(Tasks::default()),
        })
    }
    fn notify(&self) {
        (self.changed)();
    }
    fn fence(&self, scope: Scope, clear: bool, expected: &Option<String>) -> Result<u64, Fault> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if state.closed
            || !state.foreground
            || state.generation != scope.generation
            || scope.profile.to_string() != self.profile
        {
            return Err(Fault::Cancelled);
        }
        if state.loaded && state.config.as_ref().map(|c| &c.id) != expected.as_ref() {
            return Err(Fault::Changed);
        }
        state.generation = state.generation.wrapping_add(1);
        if clear {
            state.models.clear();
            state.checked_at_ms = None;
        }
        state.checking = false;
        state.quota.clear();
        self.cancellation.send_replace(state.generation);
        Ok(state.generation)
    }
    fn expected(&self, expected: &Option<String>) -> Result<(), Fault> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if state.config.as_ref().map(|c| &c.id) == expected.as_ref() {
            Ok(())
        } else {
            Err(Fault::Changed)
        }
    }
    fn admitted(&self, generation: u64) -> bool {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        !state.closed && state.foreground && state.generation == generation
    }
    /// Capture this service's current native scope without I/O or mutation.
    pub fn scope(&self) -> Result<Scope, Fault> {
        Ok(Scope {
            generation: self.admission()?,
            profile: zephium_core::ids::ProfileId::parse(&self.profile).ok_or(Fault::Invalid)?,
        })
    }
    fn scoped_generation(&self, scope: Scope) -> Result<u64, Fault> {
        if scope.profile.to_string() != self.profile || !self.admitted(scope.generation) {
            Err(Fault::Cancelled)
        } else {
            Ok(scope.generation)
        }
    }
    fn admission(&self) -> Result<u64, Fault> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        if state.closed || !state.foreground {
            Err(Fault::Cancelled)
        } else {
            Ok(state.generation)
        }
    }
    async fn owned<T: Send + 'static>(
        self: &Arc<Self>,
        erase: bool,
        future: impl Future<Output = Result<T, Fault>> + Send + 'static,
    ) -> Result<T, Fault> {
        let guard = {
            let mut tasks = self.tasks.state.lock().unwrap_or_else(|e| e.into_inner());
            if tasks.closed && !erase {
                return Err(Fault::Cancelled);
            }
            if tasks.active >= 32 {
                return Err(Fault::Busy);
            }
            tasks.active += 1;
            Guard(self.tasks.clone())
        };
        tokio::spawn(async move {
            let _guard = guard;
            future.await
        })
        .await
        .map_err(|_| Fault::Unavailable)?
    }
    async fn load_locked(&self) -> Result<(), Fault> {
        if self.state.read().unwrap_or_else(|e| e.into_inner()).loaded {
            return Ok(());
        }
        let vault = self.vault.clone();
        let profile = self.profile.clone();
        let loaded = tokio::task::spawn_blocking(move || vault.load(&profile))
            .await
            .map_err(|_| Fault::Vault)?
            .map_err(|_| Fault::Vault)?;
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(Fault::Cancelled);
        }
        state.config = loaded;
        state.loaded = true;
        Ok(())
    }
    /// Read configuration only. It does not check the OpenCode Go or infer a subscription.
    pub async fn load(self: &Arc<Self>) -> Result<(), Fault> {
        self.load_scoped(self.scope()?).await
    }
    /// Load only for the service scope atomically admitted by the native registry.
    pub async fn load_scoped(self: &Arc<Self>, scope: Scope) -> Result<(), Fault> {
        let generation = self.scoped_generation(scope)?;
        let service = self.clone();
        self.owned(false, async move {
            let _turn = service.mutation.lock().await;
            if !service.admitted(generation) {
                return Err(Fault::Cancelled);
            }
            service.load_locked().await
        })
        .await
    }
    /// Save a new OpenCode Go key through the native vault. No network request occurs.
    /// Every replacement gets a fresh identity, retiring old selections and replay scope.
    pub async fn configure(
        self: &Arc<Self>,
        expected_id: Option<String>,
        key: Zeroizing<String>,
        billing_acknowledged: bool,
    ) -> Result<(), Fault> {
        self.configure_scoped(self.scope()?, expected_id, key, billing_acknowledged)
            .await
    }
    /// Save only within the native scope captured before dispatch can be preempted.
    pub async fn configure_scoped(
        self: &Arc<Self>,
        scope: Scope,
        expected_id: Option<String>,
        key: Zeroizing<String>,
        billing_acknowledged: bool,
    ) -> Result<(), Fault> {
        let config = Config::new(
            key,
            new_id().map_err(|_| Fault::Unavailable)?,
            billing_acknowledged,
        )
        .map_err(|_| Fault::Invalid)?;
        let generation = self.fence(scope, true, &expected_id)?;
        self.notify();
        let service = self.clone();
        self.owned(false, async move {
            let _turn = service.mutation.lock().await;
            if !service.admitted(generation) {
                return Err(Fault::Cancelled);
            }
            service.load_locked().await?;
            if !service.admitted(generation) {
                return Err(Fault::Cancelled);
            }
            service.expected(&expected_id)?;
            let config = Arc::new(config);
            let saved = config.clone();
            let vault = service.vault.clone();
            let profile = service.profile.clone();
            let result = tokio::task::spawn_blocking(move || vault.save(&profile, &saved))
                .await
                .map_err(|_| Fault::Vault)?
                .map_err(|_| Fault::Vault);
            {
                let mut state = service.state.write().unwrap_or_else(|e| e.into_inner());
                // An admitted atomic save owns its completion even if its IPC waiter
                // disappears. Never publish a catalog/client after scope retirement.
                if result.is_ok() {
                    state.config = Some(config);
                    state.loaded = true;
                    state.models.clear();
                    state.checked_at_ms = None;
                    state.last_billing = None;
                }
                state.fault = result.err();
            }
            service.notify();
            result
        })
        .await
    }
    /// Explicit catalog check. Configured, reachable and model-listed remain distinct.
    pub async fn check_models(self: &Arc<Self>, expected_id: Option<String>) -> Result<(), Fault> {
        self.check_models_scoped(self.scope()?, expected_id).await
    }
    /// Check the catalog only for the atomically admitted native scope.
    pub async fn check_models_scoped(
        self: &Arc<Self>,
        scope: Scope,
        expected_id: Option<String>,
    ) -> Result<(), Fault> {
        let generation = self.scoped_generation(scope)?;
        let service = self.clone();
        self.owned(false, async move {
            let _turn = service.mutation.lock().await;
            if !service.admitted(generation) { return Err(Fault::Cancelled); }
            service.load_locked().await?;
            service.expected(&expected_id)?;
            let config = {
                let mut state = service.state.write().unwrap_or_else(|e| e.into_inner());
                if state.closed || !state.foreground || state.generation != generation { return Err(Fault::Cancelled); }
                let config = state.config.clone().ok_or(Fault::NotConfigured)?;
                state.checking = true; state.fault = None; config
            };
            service.notify(); let mut cancelled = service.cancellation.subscribe();
            if !service.admitted(generation) { return Err(Fault::Cancelled); }
            let result = tokio::select! {
                biased;
                _ = cancelled.changed() => Err(Fault::Cancelled),
                result = service.transport.models(&config) => result.map_err(|failure| Fault::from(failure.fault)),
            };
            {
                let mut state = service.state.write().unwrap_or_else(|e| e.into_inner());
                if state.closed || !state.foreground || state.generation != generation { return Err(Fault::Cancelled); }
                state.checking = false; state.checked_at_ms = now_ms();
                match &result {
                    Ok(models) => { state.models = models.clone(); state.fault = None; },
                    Err(fault) => { state.models.clear(); state.fault = Some(*fault); },
                }
            }
            service.notify(); result.map(|_| ())
        }).await
    }
    /// Revoke local clients immediately, then delete only this OpenCode Go credential.
    /// A failed delete remains an explicit storage failure; retry is safe.
    pub async fn disconnect(self: &Arc<Self>, expected_id: Option<String>) -> Result<(), Fault> {
        self.disconnect_scoped(self.scope()?, expected_id).await
    }
    /// Delete only for the atomically admitted native scope and observed connection.
    pub async fn disconnect_scoped(
        self: &Arc<Self>,
        scope: Scope,
        expected_id: Option<String>,
    ) -> Result<(), Fault> {
        let generation = self.fence(scope, true, &expected_id)?;
        self.notify();
        let service = self.clone();
        self.owned(false, async move {
            let _turn = service.mutation.lock().await;
            if !service.admitted(generation) {
                return Err(Fault::Cancelled);
            }
            service.load_locked().await?;
            if !service.admitted(generation) {
                return Err(Fault::Cancelled);
            }
            service.expected(&expected_id)?;
            service.forget_locked().await
        })
        .await
    }
    async fn forget_locked(&self) -> Result<(), Fault> {
        let vault = self.vault.clone();
        let profile = self.profile.clone();
        let result = tokio::task::spawn_blocking(move || vault.forget(&profile))
            .await
            .map_err(|_| Fault::Vault)?
            .map_err(|_| Fault::Vault);
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.models.clear();
            state.checked_at_ms = None;
            state.checking = false;
            state.quota.clear();
            state.fault = result.err();
            if result.is_ok() {
                state.config = None;
                state.last_billing = None;
                state.loaded = true;
            }
        }
        self.notify();
        result
    }
    /// Participate in crash-resumable profile deletion before the journal finalizes.
    pub async fn erase(self: &Arc<Self>) -> Result<(), Fault> {
        self.close();
        let service = self.clone();
        self.owned(true, async move {
            let _turn = service.mutation.lock().await;
            service.forget_locked().await
        })
        .await
    }
    /// Safe cached projection; IPC separately enforces the current persistent profile.
    pub fn view(&self) -> View {
        let s = self.state.read().unwrap_or_else(|e| e.into_inner());
        View {
            configured: s.config.is_some(),
            connection_id: s.config.as_ref().map(|c| c.id.clone()),
            models: s.models.len(),
            checking: s.checking,
            checked_at_ms: s.checked_at_ms,
            fault: s.fault,
            last_billing: s.last_billing.clone(),
            quota: s.quota.view(now_ms()),
        }
    }
    /// Current source-distinct catalog; never claims a OpenCode Go is free or subscribed.
    pub fn entries(&self) -> Vec<WorkModelEntry> {
        let s = self.state.read().unwrap_or_else(|e| e.into_inner());
        if s.closed || !s.foreground {
            return vec![];
        }
        let Some(config) = &s.config else {
            return vec![];
        };
        s.models
            .iter()
            .map(|m| WorkModelEntry {
                id: format!("opencode-go/{}/{}", config.id, m.id),
                model: WorkModelRef {
                    provider: WorkModelProvider::OpenCodeGo,
                    wire: m.wire,
                    model: m.id.clone(),
                },
                display_name: m.display_name.clone(),
                roles: vec![
                    WorkModelRole::Lead,
                    WorkModelRole::Page,
                    WorkModelRole::Light,
                ],
                recommended: false,
                context_window: 32768,
                max_output: 8192,
                supports: WorkModelSupports {
                    tools: true,
                    vision: false,
                    prompt_cache: false,
                    reasoning: false,
                    native_search: false,
                },
                price: None,
            })
            .collect()
    }
    /// Capture authorization only for an exact current source/model ID.
    pub fn lease(&self, model_id: &str) -> Option<Lease> {
        let s = self.state.read().unwrap_or_else(|e| e.into_inner());
        if s.closed || !s.foreground {
            return None;
        }
        let config = s.config.as_ref()?;
        let model = s
            .models
            .iter()
            .find(|m| model_id == format!("opencode-go/{}/{}", config.id, m.id))?;
        Some(Lease {
            generation: s.generation,
            model_id: model_id.into(),
            model: model.id.clone(),
            wire: model.wire,
        })
    }
    fn lease_valid(&self, lease: &Lease) -> bool {
        self.lease(&lease.model_id).as_ref() == Some(lease)
    }
    /// Dispatch one explicit user Work call, fenced by profile/configuration lifetime.
    pub async fn call(
        &self,
        lease: Lease,
        session: SessionId,
        request: WorkModelRequest,
        events: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<WorkModelOutcome, WorkModelError> {
        if request.model.provider != WorkModelProvider::OpenCodeGo
            || request.model.wire != lease.wire
            || request.model.model != lease.model
        {
            return Err(WorkModelError::OpenCodeGo {
                fault: OpenCodeGoModelFault::Unsupported,
                usage_unknown: false,
            });
        }
        if !self.lease_valid(&lease) {
            return Err(retired(false));
        }
        let config = self
            .state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .config
            .clone()
            .ok_or_else(|| retired(false))?;
        let mut cancelled = self.cancellation.subscribe();
        if !self.lease_valid(&lease) {
            return Err(retired(false));
        }
        let emit = |event| {
            if self.lease_valid(&lease) {
                events(event);
            }
        };
        let outcome = tokio::select! {
            biased;
            _ = cancelled.changed() => return Err(retired(true)),
            result = self.transport.call(&config,session,request,&emit) => result.map_err(GoFailure::into_model_error)?,
        };
        if !self.lease_valid(&lease) {
            return Err(retired(true));
        }
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.closed || !state.foreground || state.generation != lease.generation {
                return Err(retired(true));
            }
            state.last_billing = now_ms().map(|observed_at_ms| BillingObservation {
                source: outcome.billing.source,
                observed_at_ms,
            });
        }
        self.notify();
        if !self.lease_valid(&lease) {
            return Err(retired(true));
        }
        let outcome = outcome.outcome;
        for part in &outcome.assistant {
            if let WorkModelPart::ToolCall(tool) = part {
                emit(WorkModelEvent::ToolCall(tool.clone()));
            }
        }
        // Observers may synchronously retire this scope. The returned assistant
        // parts are executable proposals too, not just the callback projection.
        if !self.lease_valid(&lease) {
            return Err(retired(true));
        }
        Ok(outcome)
    }
    /// Native profile changes retire leases even on a quick away/back transition.
    pub fn set_foreground(&self, foreground: bool) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        if state.foreground && !foreground {
            state.generation = state.generation.wrapping_add(1);
            state.checking = false;
            state.quota.retire();
            self.cancellation.send_replace(state.generation);
        }
        state.foreground = foreground;
    }
    /// Stop admission, pending catalog observations and all existing model leases.
    pub fn close(&self) {
        self.tasks
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed = true;
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if !state.closed {
            state.closed = true;
            state.foreground = false;
            state.checking = false;
            state.quota.retire();
            state.generation = state.generation.wrapping_add(1);
            self.cancellation.send_replace(state.generation);
        }
    }
    /// Retired owners may leave the registry only after all owned writes settle.
    pub fn closed_and_drained(&self) -> bool {
        let tasks = self.tasks.state.lock().unwrap_or_else(|e| e.into_inner());
        tasks.closed && tasks.active == 0
    }
    /// Wait for admitted durable mutations; dropping the waiter never drops a write.
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
}
fn retired(usage_unknown: bool) -> WorkModelError {
    WorkModelError::OpenCodeGo {
        fault: OpenCodeGoModelFault::NotConfigured,
        usage_unknown,
    }
}
fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| d.as_millis().try_into().ok())
}

#[cfg(test)]
mod tests;
