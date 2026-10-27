//! Healthy physical dormancy is separate from failure and from fresh admission.
//! All wake work uses the original Service queue; no provider turn is replayed.
use super::*;
use zephium_codex_broker::connection::{DormantReceipt, DormantWakeFailure};

pub(super) struct HistoricalQuota {
    revision: u64,
    view: zephium_ipc::codex_quota::CodexQuotaViewV1,
}
pub(super) enum Custody {
    Healthy {
        receipt: DormantReceipt,
        quota: Option<Box<HistoricalQuota>>,
    },
    Failed(DormantWakeFailure),
}
/// Contains only local correlation and cancellation. The native token remains
/// in `previous` until the newly allocated worker has actually started.
pub(super) struct Startup {
    pub(super) previous: Mutex<Option<Arc<Service>>>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) deadline: Instant,
}

#[cfg(test)]
pub(super) fn retained(service: &Service) -> bool {
    service.dormant.lock().is_ok_and(|value| value.is_some())
        || service.startup.as_ref().is_some_and(|start| {
            start
                .previous
                .lock()
                .is_ok_and(|previous| previous.as_ref().is_some_and(|owner| retained(owner)))
        })
}
fn source(service: &Arc<Service>) -> Option<Arc<Service>> {
    if service.dormant.lock().is_ok_and(|value| value.is_some()) {
        Some(service.clone())
    } else {
        service
            .startup
            .as_ref()?
            .previous
            .lock()
            .ok()?
            .as_ref()
            .and_then(source)
    }
}

pub(super) fn healthy(service: &Service) -> bool {
    service.done.load(Ordering::Acquire)
        && service
            .dormant
            .lock()
            .is_ok_and(|value| matches!(&*value, Some(Custody::Healthy { .. })))
}

/// Historical same-profile reading only. No new timestamp, epoch, allowance or
/// quota authority is created by releasing an idle runtime.
pub(super) fn stale_quota(view: &mut zephium_ipc::codex_quota::CodexQuotaViewV1) {
    use zephium_ipc::codex_quota::CodexQuotaStateV1;
    if matches!(
        view.state,
        CodexQuotaStateV1::Current | CodexQuotaStateV1::Stale
    ) {
        view.state = CodexQuotaStateV1::Stale;
        view.refresh_needed = true;
    }
    view.checking = false;
}

pub(super) fn publish_returned(service: &Service, panicked: bool) {
    if panicked || !service.done.load(Ordering::Acquire) {
        return;
    }
    let Ok(custody) = service.dormant.lock() else {
        return;
    };
    let Some(Custody::Healthy { quota, .. }) = &*custody else {
        return;
    };
    let revision = service.account_revision.swap(0, Ordering::AcqRel);
    if let Ok(mut view) = service.view.lock() {
        view.runtime = CodexRuntimeV1::Dormant;
        view.text_only = true;
        view.quota = Default::default();
        if !service.stop.load(Ordering::Acquire) && view.state == CodexStateV1::Connected {
            if let Some(prior) = quota
                .as_ref()
                .filter(|prior| prior.revision == revision && revision != 0)
            {
                view.quota = prior.view.clone();
                stale_quota(&mut view.quota);
            }
        }
        view.error = None;
    }
}

pub(super) fn open(
    service: &Service,
    profile: NativeProfile,
    persistence: Box<dyn zephium_codex_broker::connection::Associations>,
    cached_release: Option<&str>,
) -> Result<Connection, Fault> {
    let Some(startup) = service.startup.as_ref() else {
        return Connection::open(
            profile,
            service.epoch,
            persistence,
            cached_release,
            &service.stop,
        );
    };
    if service.stop.load(Ordering::Acquire) || startup.cancel.load(Ordering::Acquire) {
        return Err(Fault::Cancelled);
    }
    if Instant::now() >= startup.deadline {
        return Err(Fault::Timeout);
    }
    if REGISTRY
        .get()
        .is_none_or(|registry| !registry.current(service))
    {
        return Err(Fault::ScopeChanged);
    }
    let previous =
        admission::Transfer::take(&startup.previous).map_err(|_| Fault::RecoveryRequired)?;
    let Some(previous) = previous else {
        return Connection::open_for_demand(
            profile,
            service.epoch,
            persistence,
            cached_release,
            &startup.cancel,
            startup.deadline,
        );
    };
    if previous.value().profile != service.profile || !previous.value().fully_settled() {
        return Err(Fault::ScopeChanged);
    }
    let custody = previous
        .value()
        .dormant
        .lock()
        .ok()
        .and_then(|mut value| value.take());
    let Some(custody) = custody else {
        return Err(Fault::RecoveryRequired);
    };
    // Release the local transfer lock and old Service chain before native I/O.
    drop(previous.commit());

    let result = match custody {
        Custody::Healthy { receipt, .. } => Connection::open_dormant(
            receipt,
            service.epoch,
            persistence,
            cached_release,
            &startup.cancel,
            startup.deadline,
        ),
        Custody::Failed(failure) => Connection::retry_dormant(
            failure,
            service.epoch,
            persistence,
            cached_release,
            &startup.cancel,
            startup.deadline,
        ),
    };
    result.map_err(|failure| {
        let fault = failure.fault();
        *service
            .dormant
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Custody::Failed(failure));
        fault
    })
}

/// Queue sealing and native quiescence are independent predicates. Neither a
/// timer nor worker.done can manufacture this receipt or clear a local effect.
pub(super) fn park(
    service: &Service,
    connection: Connection,
    scope: &conversation::Scope,
) -> Result<Option<Box<Connection>>, Box<Connection>> {
    let parking = match service.queue.try_park(|| {
        !service.stop.load(Ordering::Acquire)
            && service.removal.lock().is_ok_and(|value| value.is_none())
            && service.login.lock().is_ok_and(|value| value.is_none())
            && service.settling.lock().is_ok_and(|value| value.is_none())
            && service
                .browser_quarantine
                .lock()
                .is_ok_and(|value| value.is_empty())
            && connection.ready_to_dormant(scope)
    }) {
        Ok(parking) => parking,
        Err(_) => return Err(Box::new(connection)),
    };
    let historical = if let Ok(mut view) = service.view.lock() {
        use zephium_ipc::codex_quota::CodexQuotaStateV1;
        let revision = service.account_revision.load(Ordering::Acquire);
        let prior = (revision != 0
            && view.state == CodexStateV1::Connected
            && matches!(
                view.quota.state,
                CodexQuotaStateV1::Current | CodexQuotaStateV1::Stale
            ))
        .then(|| {
            Box::new(HistoricalQuota {
                revision,
                view: view.quota.clone(),
            })
        });
        view.runtime = CodexRuntimeV1::Preparing;
        view.text_only = true;
        view.quota = Default::default();
        prior
    } else {
        None
    };
    if let Some(registry) = REGISTRY.get() {
        notify_models(service, &Arc::downgrade(registry));
    }
    match connection.try_dormant(scope, Instant::now() + Duration::from_secs(30)) {
        Ok(receipt) => {
            *service
                .dormant
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Custody::Healthy {
                receipt,
                quota: historical,
            });
            // Keep old queue sealed forever. Publication happens only after the
            // worker returns, not merely when native physical finish returns.
            drop(parking);
            Ok(None)
        }
        Err(failure) => {
            let retryable = failure.retryable_busy();
            let (_, error, owner) = failure.into_parts();
            if retryable {
                parking.reopen();
                Ok(Some(owner))
            } else {
                set_fault(service, fault(error));
                drop(parking);
                Err(owner)
            }
        }
    }
}

pub(super) fn capture_failed_wake(
    service: &Service,
    connection: Connection,
    scope: &conversation::Scope,
    error: Fault,
) -> Result<(), Box<Connection>> {
    match connection.into_failed_dormant_wake(scope, error) {
        Ok(failure) => {
            *service
                .dormant
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Custody::Failed(failure));
            Ok(())
        }
        Err(owner) => Err(owner),
    }
}

/// Metadata only: no account revision, schema capability or dispatch token.
pub(super) fn page_choice(profile: ProfileId) -> Option<NativeDormantChoice> {
    let registry = REGISTRY.get()?;
    registry.scope(&profile.to_string())?;
    let service = registry.services.lock().ok()?.get(&profile)?.clone();
    if !healthy(&service) {
        return None;
    }
    let view = service.snapshot();
    if !view.page_selected || view.state != CodexStateV1::Connected {
        return None;
    }
    let selected = view.selection.as_ref()?;
    let model = view
        .models
        .iter()
        .find(|model| model.id == selected.model)?;
    Some(NativeDormantChoice {
        id: model.id.clone(),
        name: model.name.clone(),
        source: PageAssistModelSourceV1::Codex,
        effort: match selected.effort {
            CodexEffortSelectionV1::Default => None,
            CodexEffortSelectionV1::Explicit(value) => Some(effort(value).as_str().into()),
        },
    })
}

/// An explicit action carries the original preparation ceiling through the
/// same queue and every native follow-up. It cannot contain a Reply or removal.
pub(super) struct Action {
    kind: ActionKind,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    account_revision: u64,
}
enum ActionKind {
    Account,
    Models,
    Quota,
    Logout,
    Login(LoginAttempt),
}
impl Action {
    fn wrap(
        command: Command,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
        account_revision: u64,
    ) -> Command {
        let kind = match command {
            Command::Account => ActionKind::Account,
            Command::Models => ActionKind::Models,
            Command::Quota => ActionKind::Quota,
            Command::Logout => ActionKind::Logout,
            Command::Login(attempt) => ActionKind::Login(attempt),
            command => return command, // Local preferences never enter runtime preparation.
        };
        Command::Action(Self {
            kind,
            cancel,
            deadline,
            account_revision,
        })
    }
    pub(super) fn run(
        self,
        service: &Service,
        connection: &mut Connection,
        scope: &conversation::Scope,
    ) -> Result<(), Fault> {
        let current = if self.cancel.load(Ordering::Acquire) {
            Err(Fault::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(Fault::Timeout)
        } else if service.stop.load(Ordering::Acquire)
            || self.account_revision == 0
            || self.account_revision != service.account_revision.load(Ordering::Acquire)
        {
            Err(Fault::ScopeChanged)
        } else {
            Ok(())
        };
        match self.kind {
            ActionKind::Account => current.and_then(|_| {
                connection.read_account_for_demand(scope, &self.cancel, self.deadline)
            }),
            ActionKind::Models => current.and_then(|_| {
                connection.refresh_catalog_for_demand(scope, &self.cancel, self.deadline)
            }),
            ActionKind::Quota => {
                let result = current.and_then(|_| {
                    connection.request_quota_refresh_for_demand(scope, &self.cancel, self.deadline)
                });
                if let Ok(observation) = connection.quota(scope) {
                    quota(service, observation);
                }
                result
            }
            ActionKind::Logout => current.and_then(|_| {
                if let Ok(mut view) = service.view.lock() {
                    view.login_attempt = None;
                    view.selection = None;
                }
                connection.logout_for_demand(scope, &self.cancel, self.deadline)
            }),
            ActionKind::Login(attempt) => {
                let cancelled = attempt.cancel.load(Ordering::Acquire);
                let result = if cancelled {
                    Ok(())
                } else {
                    current.and_then(|_| {
                        connection.login_for_demand(
                            scope,
                            &attempt.cancel,
                            &self.cancel,
                            self.deadline,
                        )
                    })
                };
                if cancelled || result.is_err() {
                    if let Ok(mut view) = service.view.lock() {
                        if let Ok(mut pending) = service.login.lock() {
                            if pending.as_ref().is_some_and(|value| value.id == attempt.id) {
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
        }
    }
}

pub(super) struct Demand {
    service: Arc<Service>,
    held: Option<admission::Demand>,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    selection_revision: u64,
    fresh_wake: bool,
}
impl Demand {
    fn current(&self) -> Result<(), CodexFaultV1> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(CodexFaultV1::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(CodexFaultV1::Timeout);
        }
        if REGISTRY
            .get()
            .is_none_or(|registry| !registry.current(&self.service))
        {
            return Err(CodexFaultV1::ScopeChanged);
        }
        Ok(())
    }
    pub(super) fn command(mut self, value: Command) -> Result<(), CodexFaultV1> {
        self.current()?;
        if self.fresh_wake && matches!(&value, Command::Account) {
            // This exact demand just awaited the newly verified owner's fresh
            // account/catalog result. Check does not issue a redundant read.
            return Ok(());
        }
        let held = self.held.take().ok_or(CodexFaultV1::ScopeChanged)?;
        let value = Action::wrap(
            value,
            self.cancel.clone(),
            self.deadline,
            self.service.account_revision.load(Ordering::Acquire),
        );
        self.service
            .queue
            .handoff_command(held, value)
            .map_err(|_| CodexFaultV1::Busy)
    }
    pub(super) fn service(&self) -> Arc<Service> {
        self.service.clone()
    }
}
impl NativeConversationDemand for Demand {
    fn reserve_browser(
        mut self: Box<Self>,
        expected: NativeConversationChoice,
        draft: NativeConversationDraft,
        manifest: NativeBrowserManifest,
    ) -> BoxFuture<'static, Result<Box<dyn NativeConversationReservation>, PageBackendError>> {
        Box::pin(async move {
            self.current().map_err(backend)?;
            if self.selection_revision != self.service.selection_revision.load(Ordering::Acquire) {
                return Err(backend(CodexFaultV1::ScopeChanged));
            }
            let held = self.held.take().ok_or(PageBackendError::Busy)?;
            let reserved = self
                .service
                .queue
                .refine_demand(held)
                .map_err(|_| PageBackendError::Busy)?;
            let base = reservation_from_held(self.service.clone(), expected, draft, reserved)?;
            browser_reservation::reserve_held(base, manifest, self.cancel.clone(), self.deadline)
                .await
        })
    }
}
fn backend(error: CodexFaultV1) -> PageBackendError {
    if error == CodexFaultV1::Busy {
        return PageBackendError::Busy;
    }
    PageBackendError::NativeConversation(match error {
        CodexFaultV1::ScopeChanged => PageAssistNativeFaultV1::ScopeChanged,
        CodexFaultV1::Cancelled => PageAssistNativeFaultV1::Cancelled,
        CodexFaultV1::Timeout => PageAssistNativeFaultV1::Timeout,
        _ => PageAssistNativeFaultV1::Unavailable,
    })
}

impl Registry {
    fn explicit_demand(
        self: &Arc<Self>,
        profile: ProfileId,
        epoch: u64,
        mode: admission::TurnMode,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
        retry_failed: bool,
    ) -> Result<(Arc<Service>, admission::Demand, bool), CodexFaultV1> {
        let mut services = self
            .services
            .lock()
            .map_err(|_| CodexFaultV1::Unavailable)?;
        if self.scope(&profile.to_string()) != Some(epoch) || cancel.load(Ordering::Acquire) {
            return Err(CodexFaultV1::ScopeChanged);
        }
        if Instant::now() >= deadline {
            return Err(CodexFaultV1::Timeout);
        }
        if !self.removals.admission_open() || self.removals.contains(profile) {
            return Err(CodexFaultV1::RecoveryRequired);
        }
        let previous = if let Some(service) = services.get(&profile) {
            if !service.done.load(Ordering::Acquire) {
                if service.epoch != epoch || service.stop.load(Ordering::Acquire) {
                    return Err(CodexFaultV1::Busy);
                }
                let held = match mode {
                    admission::TurnMode::Browser => service.queue.demand_browser(),
                    admission::TurnMode::Text => service.queue.demand_command(),
                }
                .map_err(|_| CodexFaultV1::Busy)?;
                return Ok((service.clone(), held, false));
            }
            if !service.fully_settled() {
                return Err(CodexFaultV1::RecoveryRequired);
            }
            match source(service) {
                Some(previous) => Some(previous),
                None if retry_failed => None,
                None => return Err(CodexFaultV1::Unavailable),
            }
        } else {
            None
        };
        if services.len() >= 64 && !services.contains_key(&profile) {
            return Err(CodexFaultV1::Busy);
        }
        let (service, held) = self.spawn_started(
            &mut services,
            profile,
            epoch,
            SpawnPlan {
                startup: Some(Startup {
                    previous: Mutex::new(previous),
                    cancel,
                    deadline,
                }),
                demand_mode: Some(mode),
                ..Default::default()
            },
        )?;
        Ok((service, held.ok_or(CodexFaultV1::Unavailable)?, true))
    }
}

pub(super) async fn prepare(
    profile: ProfileId,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    mode: admission::TurnMode,
    retry_failed: bool,
) -> Result<Demand, CodexFaultV1> {
    let registry = REGISTRY.get().ok_or(CodexFaultV1::Unavailable)?;
    let epoch = registry
        .scope(&profile.to_string())
        .ok_or(CodexFaultV1::ScopeChanged)?;
    let selection_revision = read_selection(profile)?.revision;
    let deadline = deadline.min(Instant::now() + Duration::from_secs(120));
    let mut cancellation = CancelReply(Some(cancel.clone()));
    let (service, held, fresh_wake) = loop {
        match registry.explicit_demand(profile, epoch, mode, cancel.clone(), deadline, retry_failed)
        {
            Ok(value) => break value,
            Err(CodexFaultV1::Busy) => {
                // Wait only for the already sealed healthy-close transition,
                // never retry an occupied turn, command or failed owner.
                let parking = registry
                    .services
                    .lock()
                    .ok()
                    .and_then(|owners| owners.get(&profile).cloned())
                    .is_some_and(|service| {
                        service.queue.is_parked() && !service.done.load(Ordering::Acquire)
                    });
                if !parking {
                    return Err(CodexFaultV1::Busy);
                }
                if cancel.load(Ordering::Acquire) {
                    return Err(CodexFaultV1::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(CodexFaultV1::Timeout);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    };
    service
        .cancellations
        .register(&cancel)
        .map_err(|error| match error {
            admission::Error::Busy => CodexFaultV1::Busy,
            admission::Error::Closed => CodexFaultV1::ScopeChanged,
        })?;
    let demand = Demand {
        service,
        held: Some(held),
        cancel,
        deadline,
        selection_revision,
        fresh_wake,
    };
    loop {
        demand.current()?;
        let view = demand.service.snapshot();
        if view.runtime == CodexRuntimeV1::Active {
            if demand.service.selection_revision.load(Ordering::Acquire) != selection_revision {
                return Err(CodexFaultV1::ScopeChanged);
            }
            cancellation.0 = None;
            return Ok(demand);
        }
        if view.runtime == CodexRuntimeV1::Unavailable {
            return Err(view.error.unwrap_or(CodexFaultV1::Unavailable));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) async fn prepare_page(
    profile: ProfileId,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Option<Box<dyn NativeConversationDemand>>, PageBackendError> {
    let selected = read_selection(profile).map_err(backend)?;
    if selected.backend == page_choice::Backend::Model {
        return Ok(None);
    }
    let demand = prepare(
        profile,
        cancel,
        deadline,
        admission::TurnMode::Browser,
        false,
    )
    .await
    .map_err(backend)?;
    Ok(Some(Box::new(demand)))
}
