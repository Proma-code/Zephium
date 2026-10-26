//! Windows-only explicit OpenCode Go access-key connection.
//! No CLI/OAuth import, automatic model selection or paid fallback.
use super::*;
use zephium_ipc::opencode_go::{GoCommand, GoViewV1};

#[cfg(all(windows, feature = "work-product"))]
mod live {
    use super::*;
    use std::collections::HashMap;
    use zephium_agentic::lead::opencode_go::{BillingSource, SessionId};
    use zephium_core::ids::ProfileId;
    use zephium_core::work::model::*;
    use zephium_ipc::opencode_go::{
        GoBillingSourceV1, GoBillingViewV1, GoFaultV1, GoQuotaFaultV1, GoQuotaKindV1,
        GoQuotaStateV1, GoQuotaViewV1, GoQuotaWindowV1, GoStateV1,
    };
    use zephium_opencode_go::service::quota::{QuotaFault, QuotaStatus, QuotaView};
    use zephium_opencode_go::service::{Fault, Service};
    use zeroize::Zeroizing;

    #[cfg(feature = "windows-subscription-trial")]
    const VAULT_NAMESPACE: &str = "app.zephium.subscription-trial";
    #[cfg(not(feature = "windows-subscription-trial"))]
    const VAULT_NAMESPACE: &str = "app.zephium";
    static REGISTRY: OnceLock<Arc<Registry>> = OnceLock::new();

    struct Registry {
        services: Mutex<HashMap<ProfileId, Arc<Service>>>,
        foreground: std::sync::RwLock<Option<String>>,
        scope_epoch: std::sync::atomic::AtomicU64,
        closed: std::sync::atomic::AtomicBool,
    }
    impl Registry {
        // Read identity and epoch under one foreground lock so an A→B→A
        // transition cannot revive a command awaiting the actor profile view.
        fn capture_scope(&self, expected: &str) -> Option<u64> {
            let current = self.foreground.read().ok()?;
            (!self.closed.load(std::sync::atomic::Ordering::Acquire)
                && current.as_deref() == Some(expected))
            .then(|| self.scope_epoch.load(std::sync::atomic::Ordering::Acquire))
        }
        fn owns_scope(&self, expected: &str, epoch: u64) -> bool {
            self.capture_scope(expected) == Some(epoch)
        }

        // Check the registry epoch and capture the service generation under the
        // same profile read lock. A concurrent away/back transition cannot pass
        // an old IPC request a newly current service authority.
        fn admit_scope(
            &self,
            expected: &str,
            epoch: u64,
            service: &Service,
        ) -> Option<zephium_opencode_go::service::Scope> {
            let current = self.foreground.read().ok()?;
            if self.closed.load(std::sync::atomic::Ordering::Acquire)
                || current.as_deref() != Some(expected)
                || self.scope_epoch.load(std::sync::atomic::Ordering::Acquire) != epoch
            {
                return None;
            }
            service.scope().ok()
        }
        fn get(&self, profile: ProfileId) -> Option<Arc<Service>> {
            let services = self.services.lock().ok()?;
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            services.get(&profile).cloned()
        }
        fn close(&self) {
            let services = super::super::subscription_registry::close(
                &self.services,
                &self.foreground,
                &self.scope_epoch,
                &self.closed,
            );
            for service in services {
                service.close();
            }
        }
        fn service(
            &self,
            profile: ProfileId,
            require_foreground: bool,
        ) -> Result<Arc<Service>, Fault> {
            let mut services = self.services.lock().map_err(|_| Fault::Vault)?;
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(Fault::Cancelled);
            }
            if let Some(service) = services.get(&profile) {
                return Ok(service.clone());
            }
            if require_foreground
                && self
                    .foreground
                    .read()
                    .map_err(|_| Fault::Cancelled)?
                    .as_deref()
                    != Some(profile.to_string().as_str())
            {
                return Err(Fault::Cancelled);
            }
            services.retain(|_, service| !service.closed_and_drained());
            if services.len() >= 64 {
                return Err(Fault::Busy);
            }
            let service = Arc::new(Service::new(
                profile.to_string(),
                Arc::new(
                    zephium_opencode_go::vault::SystemVault::new(VAULT_NAMESPACE)
                        .map_err(|_| Fault::Vault)?,
                ),
                Arc::new(zephium_app::work_models::go_session_changed),
            )?);
            service.set_foreground(
                self.foreground.read().ok().is_some_and(|current| {
                    current.as_deref() == Some(profile.to_string().as_str())
                }),
            );
            services.insert(profile, service.clone());
            Ok(service)
        }
    }

    struct GoModelClient {
        service: Arc<Service>,
        model: WorkModelRef,
        session: Option<SessionId>,
        lease: zephium_opencode_go::service::Lease,
        /// This client belongs to one explicit Work run; failure never reopens it.
        terminal: tokio::sync::Mutex<Option<WorkModelError>>,
    }
    impl WorkModelClient for GoModelClient {
        fn call<'a>(
            &'a self,
            request: WorkModelRequest,
            events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
        ) -> WorkModelFuture<'a> {
            Box::pin(async move {
                if request.model != self.model || self.session.is_none() {
                    return Err(WorkModelError::OpenCodeGo {
                        fault: OpenCodeGoModelFault::Unsupported,
                        usage_unknown: false,
                    });
                }
                let mut terminal = self.terminal.lock().await;
                if let Some(error) = *terminal {
                    return Err(error);
                }
                // Dropping an in-flight future leaves this latch set. A fresh
                // explicit Work run can acquire a new client, not this agent loop.
                *terminal = Some(WorkModelError::OpenCodeGo {
                    fault: OpenCodeGoModelFault::Unavailable,
                    usage_unknown: true,
                });
                let session = self.session.clone().ok_or(WorkModelError::OpenCodeGo {
                    fault: OpenCodeGoModelFault::Unsupported,
                    usage_unknown: false,
                })?;
                let result = self
                    .service
                    .call(self.lease.clone(), session, request, events)
                    .await;
                *terminal = match &result {
                    Ok(_) => None,
                    Err(error @ WorkModelError::OpenCodeGo { .. }) => Some(*error),
                    Err(_) => Some(WorkModelError::OpenCodeGo {
                        fault: OpenCodeGoModelFault::Unavailable,
                        usage_unknown: true,
                    }),
                };
                result
            })
        }
    }
    impl zephium_app::work_models::WorkGoSession for Registry {
        fn erase_profile(&self, profile: ProfileId, deadline: std::time::Instant) -> bool {
            let Ok(service) = self.service(profile, false) else {
                return false;
            };
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tauri::async_runtime::block_on(async move {
                tokio::time::timeout(remaining, service.erase())
                    .await
                    .is_ok_and(|result| result.is_ok())
            })
        }
        fn entries(&self, profile: ProfileId) -> Vec<WorkModelEntry> {
            self.get(profile)
                .map(|service| service.entries())
                .unwrap_or_default()
        }
        fn client(
            &self,
            profile: ProfileId,
            model_id: &str,
            session: Option<String>,
        ) -> Result<Arc<dyn WorkModelClient>, WorkModelError> {
            let inactive = WorkModelError::OpenCodeGo {
                fault: OpenCodeGoModelFault::NotConfigured,
                usage_unknown: false,
            };
            let service = self.get(profile).ok_or(inactive)?;
            let entry = service
                .entries()
                .into_iter()
                .find(|entry| entry.id == model_id)
                .ok_or(inactive)?;
            let lease = service.lease(model_id).ok_or(inactive)?;
            Ok(Arc::new(GoModelClient {
                service,
                model: entry.model,
                session: session.map(SessionId::parse).transpose().map_err(|_| {
                    WorkModelError::OpenCodeGo {
                        fault: OpenCodeGoModelFault::Unsupported,
                        usage_unknown: false,
                    }
                })?,
                lease,
                terminal: tokio::sync::Mutex::new(None),
            }))
        }
    }
    pub(super) fn install() {
        let registry = REGISTRY.get_or_init(|| {
            Arc::new(Registry {
                services: Mutex::new(HashMap::new()),
                foreground: std::sync::RwLock::new(None),
                scope_epoch: std::sync::atomic::AtomicU64::new(0),
                closed: std::sync::atomic::AtomicBool::new(false),
            })
        });
        zephium_app::work_models::install_go_session(registry.clone());
    }
    pub(super) fn profiles_changed(view: &zephium_ipc::ProfilesView) {
        let Some(registry) = REGISTRY.get() else {
            return;
        };
        let current = (!view.private).then(|| view.current.clone()).flatten();
        let mut retired = Vec::new();
        {
            let Ok(services) = registry.services.lock() else {
                return;
            };
            if registry.closed.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            if let Ok(mut foreground) = registry.foreground.write() {
                if *foreground != current {
                    registry
                        .scope_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    *foreground = current.clone();
                }
            }
            for (id, service) in services.iter() {
                let present = view.profiles.iter().any(|entry| {
                    entry.id == id.to_string()
                        && entry.kind != zephium_ipc::ProfileKindView::Incognito
                });
                if !present {
                    retired.push(service.clone());
                } else {
                    service.set_foreground(current.as_deref() == Some(id.to_string().as_str()));
                }
            }
        }
        for service in retired {
            service.close();
        }
        zephium_app::work_models::go_session_changed();
    }
    pub(super) fn stop() {
        if let Some(registry) = REGISTRY.get() {
            registry.close();
        }
    }
    pub(super) async fn drained() -> bool {
        let Some(registry) = REGISTRY.get() else {
            return true;
        };
        if !registry.closed.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        let services = match registry.services.lock() {
            Ok(services) => services.values().cloned().collect::<Vec<_>>(),
            Err(_) => return false,
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async move {
            for service in services {
                service.drained().await;
            }
        })
        .await
        .is_ok()
    }
    fn fault(fault: Fault) -> GoFaultV1 {
        match fault {
            Fault::Invalid => GoFaultV1::Invalid,
            Fault::Unavailable => GoFaultV1::Unavailable,
            Fault::Vault => GoFaultV1::Vault,
            Fault::NotConfigured => GoFaultV1::NotConfigured,
            Fault::Changed => GoFaultV1::Changed,
            Fault::Busy => GoFaultV1::Busy,
            Fault::Cancelled => GoFaultV1::Cancelled,
            Fault::Unauthorized => GoFaultV1::Unauthorized,
            Fault::Forbidden => GoFaultV1::Forbidden,
            Fault::RateLimited => GoFaultV1::RateLimited,
            Fault::Protocol => GoFaultV1::Protocol,
            Fault::Incomplete => GoFaultV1::Incomplete,
            Fault::UsageUnavailable => GoFaultV1::UsageUnavailable,
        }
    }
    fn quota_fault(fault: QuotaFault) -> GoQuotaFaultV1 {
        match fault {
            QuotaFault::Invalid => GoQuotaFaultV1::Invalid,
            QuotaFault::NotConfigured => GoQuotaFaultV1::NotConfigured,
            QuotaFault::Changed => GoQuotaFaultV1::Changed,
            QuotaFault::Vault => GoQuotaFaultV1::Vault,
            QuotaFault::Busy => GoQuotaFaultV1::Busy,
            QuotaFault::Cancelled => GoQuotaFaultV1::Cancelled,
            QuotaFault::Unauthorized => GoQuotaFaultV1::Unauthorized,
            QuotaFault::NoEntitlement => GoQuotaFaultV1::NoEntitlement,
            QuotaFault::Forbidden => GoQuotaFaultV1::Forbidden,
            QuotaFault::EndpointUnavailable => GoQuotaFaultV1::EndpointUnavailable,
            QuotaFault::RateLimited => GoQuotaFaultV1::RateLimited,
            QuotaFault::Timeout => GoQuotaFaultV1::Timeout,
            QuotaFault::Unavailable => GoQuotaFaultV1::Unavailable,
            QuotaFault::Protocol => GoQuotaFaultV1::Protocol,
            QuotaFault::TooLarge => GoQuotaFaultV1::TooLarge,
        }
    }
    fn quota_view(current: QuotaView, error: Option<QuotaFault>) -> GoQuotaViewV1 {
        GoQuotaViewV1 {
            state: match current.status {
                QuotaStatus::NotChecked => GoQuotaStateV1::NotChecked,
                QuotaStatus::Current => GoQuotaStateV1::Current,
                QuotaStatus::Stale => GoQuotaStateV1::Stale,
                QuotaStatus::Unavailable => GoQuotaStateV1::Unavailable,
                QuotaStatus::NoEntitlement => GoQuotaStateV1::NoEntitlement,
            },
            checking: current.checking,
            checked_at_ms: current.checked_at_ms.map(|ms| ms.to_string()),
            attempted_at_ms: current.attempted_at_ms.map(|ms| ms.to_string()),
            stale_at_ms: current.stale_at_ms.map(|ms| ms.to_string()),
            windows: current
                .reading
                .map(|reading| {
                    [
                        (GoQuotaKindV1::Rolling, reading.rolling),
                        (GoQuotaKindV1::Weekly, reading.weekly),
                        (GoQuotaKindV1::Monthly, reading.monthly),
                    ]
                    .into_iter()
                    .map(|(kind, window)| GoQuotaWindowV1 {
                        kind,
                        used_percent: window.used_percent,
                        remaining_percent: window.remaining_percent,
                        resets_at_ms: window.resets_at_ms.to_string(),
                        limited: window.limited,
                    })
                    .collect()
                })
                .unwrap_or_default(),
            error: error.or(current.fault).map(quota_fault),
        }
    }
    fn view(
        profile: String,
        service: &Service,
        error: Option<Fault>,
        quota_error: Option<QuotaFault>,
    ) -> GoViewV1 {
        let current = service.view();
        let error = error.or(current.fault);
        GoViewV1 {
            version: 1,
            profile,
            state: if !current.configured {
                GoStateV1::Unconfigured
            } else if current.checked_at_ms.is_some() && error.is_none() {
                GoStateV1::Catalogued
            } else {
                GoStateV1::Configured
            },
            connection_id: current.connection_id,
            models: current.models.try_into().unwrap_or(0),
            checking: current.checking,
            catalog_checked_at_ms: current.checked_at_ms.map(|ms| ms.to_string()),
            last_billing: current.last_billing.map(|billing| GoBillingViewV1 {
                source: match billing.source {
                    BillingSource::Free => GoBillingSourceV1::Free,
                    BillingSource::Balance => GoBillingSourceV1::Balance,
                    BillingSource::Go => GoBillingSourceV1::Go,
                    BillingSource::GoPlus => GoBillingSourceV1::GoPlus,
                    BillingSource::Byok => GoBillingSourceV1::Byok,
                },
                observed_at_ms: billing.observed_at_ms.to_string(),
            }),
            quota: quota_view(current.quota, quota_error),
            error: error.map(fault),
        }
    }
    async fn allowed(
        caller: &WebviewWindow,
        expected: &str,
        action: GoCommand,
    ) -> Option<ProfileId> {
        let profile = ProfileId::parse(expected).filter(|id| id.to_string() == expected)?;
        let app = caller.app_handle();
        if !authorize(caller, CallerPolicy::Main, action.native()) || shutdown_started(app) {
            return None;
        }
        let receiver = app.state::<Handle>().profiles();
        let profiles = tokio::task::spawn_blocking(move || {
            receiver.recv_timeout(std::time::Duration::from_secs(4))
        })
        .await
        .ok()?
        .ok()??;
        if shutdown_started(app)
            || !profiles.profiles.iter().any(|row| {
                row.id == expected && row.kind != zephium_ipc::ProfileKindView::Incognito
            })
        {
            return None;
        }
        if profiles.private || profiles.current.as_deref() != Some(expected) {
            return None;
        }
        Some(profile)
    }
    pub(super) async fn dispatch(
        caller: WebviewWindow,
        expected: String,
        action: GoCommand,
        expected_connection_id: Option<String>,
        save: Option<(Zeroizing<String>, bool)>,
    ) -> GoViewV1 {
        let Some(registry) = REGISTRY.get() else {
            return GoViewV1::unavailable(expected);
        };
        let Some(scope_epoch) = registry.capture_scope(&expected) else {
            return GoViewV1::unavailable(expected);
        };
        let Some(profile) = allowed(&caller, &expected, action).await else {
            return GoViewV1::unavailable(expected);
        };
        if !registry.owns_scope(&expected, scope_epoch) {
            return GoViewV1::unavailable(expected);
        }
        let service = match registry.service(profile, true) {
            Ok(service) => service,
            Err(error) => {
                let mut view = GoViewV1::unavailable(expected);
                view.error = Some(fault(error));
                return view;
            }
        };
        let Some(service_scope) = registry.admit_scope(&expected, scope_epoch, &service) else {
            return GoViewV1::unavailable(expected);
        };
        let mut quota_error = None;
        let result = match (action, save) {
            (GoCommand::Status, None) => service.load_scoped(service_scope).await,
            (GoCommand::Save, Some((key, acknowledged))) => {
                service
                    .configure_scoped(service_scope, expected_connection_id, key, acknowledged)
                    .await
            }
            (GoCommand::Check, None) => {
                service
                    .check_models_scoped(service_scope, expected_connection_id)
                    .await
            }
            (GoCommand::Quota, None) => {
                quota_error = service
                    .check_quota_scoped(service_scope, expected_connection_id)
                    .await
                    .err();
                Ok(())
            }
            (GoCommand::Disconnect, None) => {
                service
                    .disconnect_scoped(service_scope, expected_connection_id)
                    .await
            }
            _ => Err(Fault::Invalid),
        };
        if !registry.owns_scope(&expected, scope_epoch) {
            return GoViewV1::unavailable(expected);
        }
        view(expected, &service, result.err(), quota_error)
    }
    #[cfg(test)]
    mod registry_tests {
        use super::*;

        #[test]
        fn subscription_registry_cannot_create_first_service_after_shutdown() {
            let profile = ProfileId::parse("00000000000000000000000001").unwrap();
            let registry = Registry {
                services: Mutex::new(HashMap::new()),
                foreground: std::sync::RwLock::new(Some(profile.to_string())),
                scope_epoch: std::sync::atomic::AtomicU64::new(0),
                closed: std::sync::atomic::AtomicBool::new(false),
            };
            let old_scope = registry.capture_scope(&profile.to_string()).unwrap();
            registry.close();
            assert!(!registry.owns_scope(&profile.to_string(), old_scope));
            assert!(matches!(
                registry.service(profile, true),
                Err(Fault::Cancelled)
            ));
            assert!(matches!(
                registry.service(profile, false),
                Err(Fault::Cancelled)
            ));
            assert!(registry.get(profile).is_none());
            assert!(registry.services.lock().unwrap().is_empty());
            registry.close();
            assert!(registry.capture_scope(&profile.to_string()).is_none());
        }
    }
}

pub(crate) fn install() {
    #[cfg(all(windows, feature = "work-product"))]
    live::install();
}
pub(crate) fn profiles_changed(view: &zephium_ipc::ProfilesView) {
    #[cfg(all(windows, feature = "work-product"))]
    live::profiles_changed(view);
    #[cfg(not(all(windows, feature = "work-product")))]
    let _ = view;
}
pub(crate) fn stop() {
    #[cfg(all(windows, feature = "work-product"))]
    live::stop();
}
pub(crate) async fn drained() -> bool {
    #[cfg(all(windows, feature = "work-product"))]
    {
        live::drained().await
    }
    #[cfg(not(all(windows, feature = "work-product")))]
    {
        true
    }
}
async fn dispatch(
    caller: WebviewWindow,
    expected: String,
    action: GoCommand,
    expected_connection_id: Option<String>,
    save: Option<(String, bool)>,
) -> GoViewV1 {
    #[cfg(all(windows, feature = "work-product"))]
    {
        // Convert the sole key input before the first suspension; no Debug,
        // provider-account lookup, token parsing or command execution is used.
        let save = save.map(|(key, acknowledged)| (zeroize::Zeroizing::new(key), acknowledged));
        live::dispatch(caller, expected, action, expected_connection_id, save).await
    }
    #[cfg(not(all(windows, feature = "work-product")))]
    {
        let _ = (caller, action, expected_connection_id, save);
        GoViewV1::unavailable(expected)
    }
}
#[tauri::command]
pub(crate) async fn go_status(caller: WebviewWindow, expected_profile: String) -> GoViewV1 {
    dispatch(caller, expected_profile, GoCommand::Status, None, None).await
}
#[tauri::command]
pub(crate) async fn go_save(
    caller: WebviewWindow,
    expected_profile: String,
    expected_connection_id: Option<String>,
    key: String,
    billing_acknowledged: bool,
) -> GoViewV1 {
    dispatch(
        caller,
        expected_profile,
        GoCommand::Save,
        expected_connection_id,
        Some((key, billing_acknowledged)),
    )
    .await
}
#[tauri::command]
pub(crate) async fn go_check(
    caller: WebviewWindow,
    expected_profile: String,
    expected_connection_id: String,
) -> GoViewV1 {
    dispatch(
        caller,
        expected_profile,
        GoCommand::Check,
        Some(expected_connection_id),
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn go_quota(
    caller: WebviewWindow,
    expected_profile: String,
    expected_connection_id: String,
) -> GoViewV1 {
    dispatch(
        caller,
        expected_profile,
        GoCommand::Quota,
        Some(expected_connection_id),
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn go_disconnect(
    caller: WebviewWindow,
    expected_profile: String,
    expected_connection_id: String,
) -> GoViewV1 {
    dispatch(
        caller,
        expected_profile,
        GoCommand::Disconnect,
        Some(expected_connection_id),
        None,
    )
    .await
}
