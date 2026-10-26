//! Windows-only, main-chrome connection to an explicitly configured local gateway.
//! No gateway installation, provider OAuth, credential import or automatic prompts.
use super::*;
use zephium_ipc::local_gateway::{GatewayCommand, GatewayViewV1};

#[cfg(all(windows, feature = "work-product"))]
mod live {
    use super::*;
    use std::collections::HashMap;
    use zephium_core::ids::ProfileId;
    use zephium_core::work::model::*;
    use zephium_gateway::service::{Fault, Service};
    use zephium_ipc::local_gateway::{GatewayFaultV1, GatewayStateV1};
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
        ) -> Option<zephium_gateway::service::Scope> {
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
                    zephium_gateway::vault::SystemVault::new(VAULT_NAMESPACE)
                        .map_err(|_| Fault::Vault)?,
                ),
                Arc::new(|| zephium_app::work_models::gateway_session_changed()),
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

    struct GatewayModelClient {
        service: Arc<Service>,
        model: WorkModelRef,
        lease: zephium_gateway::service::Lease,
        /// This client belongs to one explicit Work run; failure never reopens it.
        terminal: tokio::sync::Mutex<Option<WorkModelError>>,
    }
    impl WorkModelClient for GatewayModelClient {
        fn call<'a>(
            &'a self,
            request: WorkModelRequest,
            events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
        ) -> WorkModelFuture<'a> {
            Box::pin(async move {
                if request.model != self.model {
                    return Err(WorkModelError::Gateway {
                        fault: GatewayModelFault::Unsupported,
                        usage_unknown: false,
                    });
                }
                let mut terminal = self.terminal.lock().await;
                if let Some(error) = *terminal {
                    return Err(error);
                }
                // Dropping an in-flight future leaves this latch set. A fresh
                // explicit Work run can acquire a new client, not this agent loop.
                *terminal = Some(WorkModelError::Gateway {
                    fault: GatewayModelFault::Unavailable,
                    usage_unknown: true,
                });
                let result = self.service.call(self.lease.clone(), request, events).await;
                *terminal = match &result {
                    Ok(_) => None,
                    Err(error @ WorkModelError::Gateway { .. }) => Some(*error),
                    Err(_) => Some(WorkModelError::Gateway {
                        fault: GatewayModelFault::Unavailable,
                        usage_unknown: true,
                    }),
                };
                result
            })
        }
    }
    impl zephium_app::work_models::WorkGatewaySession for Registry {
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
        ) -> Result<Arc<dyn WorkModelClient>, WorkModelError> {
            let inactive = WorkModelError::Gateway {
                fault: GatewayModelFault::NotConfigured,
                usage_unknown: false,
            };
            let service = self.get(profile).ok_or(inactive)?;
            let entry = service
                .entries()
                .into_iter()
                .find(|entry| entry.id == model_id)
                .ok_or(inactive)?;
            let lease = service.lease(model_id).ok_or(inactive)?;
            Ok(Arc::new(GatewayModelClient {
                service,
                model: entry.model,
                lease,
                terminal: tokio::sync::Mutex::new(None),
            }))
        }
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

    pub(super) fn install() {
        let registry = REGISTRY.get_or_init(|| {
            Arc::new(Registry {
                services: Mutex::new(HashMap::new()),
                foreground: std::sync::RwLock::new(None),
                scope_epoch: std::sync::atomic::AtomicU64::new(0),
                closed: std::sync::atomic::AtomicBool::new(false),
            })
        });
        zephium_app::work_models::install_gateway_session(registry.clone());
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
        zephium_app::work_models::gateway_session_changed();
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
    fn fault(fault: Fault) -> GatewayFaultV1 {
        match fault {
            Fault::Invalid => GatewayFaultV1::Invalid,
            Fault::Unavailable => GatewayFaultV1::Unavailable,
            Fault::Vault => GatewayFaultV1::Vault,
            Fault::NotConfigured => GatewayFaultV1::NotConfigured,
            Fault::Changed => GatewayFaultV1::Changed,
            Fault::Busy => GatewayFaultV1::Busy,
            Fault::Cancelled => GatewayFaultV1::Cancelled,
            Fault::Unauthorized => GatewayFaultV1::Unauthorized,
            Fault::Forbidden => GatewayFaultV1::Forbidden,
            Fault::RateLimited => GatewayFaultV1::RateLimited,
            Fault::Protocol => GatewayFaultV1::Protocol,
            Fault::Incomplete => GatewayFaultV1::Incomplete,
            Fault::UsageUnavailable => GatewayFaultV1::UsageUnavailable,
        }
    }
    fn view(profile: String, service: &Service, error: Option<Fault>) -> GatewayViewV1 {
        let current = service.view();
        let error = error.or(current.fault);
        GatewayViewV1 {
            version: 1,
            profile,
            state: if !current.configured {
                GatewayStateV1::Unconfigured
            } else if current.checked_at_ms.is_some() && error.is_none() {
                GatewayStateV1::Checked
            } else {
                GatewayStateV1::Configured
            },
            connection_id: current.connection_id,
            label: current.label,
            base: current.base,
            models: current.models.try_into().unwrap_or(0),
            checking: current.checking,
            checked_at_ms: current.checked_at_ms.map(|ms| ms.to_string()),
            error: error.map(fault),
        }
    }
    async fn allowed(
        caller: &WebviewWindow,
        expected: &str,
        action: GatewayCommand,
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
        action: GatewayCommand,
        expected_connection_id: Option<String>,
        save: Option<(String, String, Zeroizing<String>)>,
    ) -> GatewayViewV1 {
        let Some(registry) = REGISTRY.get() else {
            return GatewayViewV1::unavailable(expected);
        };
        let Some(scope_epoch) = registry.capture_scope(&expected) else {
            return GatewayViewV1::unavailable(expected);
        };
        let Some(profile) = allowed(&caller, &expected, action).await else {
            return GatewayViewV1::unavailable(expected);
        };
        if !registry.owns_scope(&expected, scope_epoch) {
            return GatewayViewV1::unavailable(expected);
        }
        let service = match registry.service(profile, true) {
            Ok(service) => service,
            Err(error) => {
                let mut view = GatewayViewV1::unavailable(expected);
                view.error = Some(fault(error));
                return view;
            }
        };
        let Some(service_scope) = registry.admit_scope(&expected, scope_epoch, &service) else {
            return GatewayViewV1::unavailable(expected);
        };
        let result = match (action, save) {
            (GatewayCommand::Status, None) => service.load_scoped(service_scope).await,
            (GatewayCommand::Save, Some((label, base, key))) => {
                service
                    .configure_scoped(service_scope, expected_connection_id, label, base, key)
                    .await
            }
            (GatewayCommand::Check, None) => {
                service
                    .check_models_scoped(service_scope, expected_connection_id)
                    .await
            }
            (GatewayCommand::Disconnect, None) => {
                service
                    .disconnect_scoped(service_scope, expected_connection_id)
                    .await
            }
            _ => Err(Fault::Invalid),
        };
        if !registry.owns_scope(&expected, scope_epoch) {
            return GatewayViewV1::unavailable(expected);
        }
        view(expected, &service, result.err())
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
    action: GatewayCommand,
    expected_connection_id: Option<String>,
    save: Option<(String, String, String)>,
) -> GatewayViewV1 {
    #[cfg(all(windows, feature = "work-product"))]
    {
        // Convert the sole key input before the first suspension; no Debug,
        // provider-account lookup, token parsing or command execution is used.
        let save = save.map(|(label, base, key)| (label, base, zeroize::Zeroizing::new(key)));
        live::dispatch(caller, expected, action, expected_connection_id, save).await
    }
    #[cfg(not(all(windows, feature = "work-product")))]
    {
        let _ = (caller, action, expected_connection_id, save);
        GatewayViewV1::unavailable(expected)
    }
}
#[tauri::command]
pub(crate) async fn gateway_status(
    caller: WebviewWindow,
    expected_profile: String,
) -> GatewayViewV1 {
    dispatch(caller, expected_profile, GatewayCommand::Status, None, None).await
}
#[tauri::command]
pub(crate) async fn gateway_save(
    caller: WebviewWindow,
    expected_profile: String,
    expected_connection_id: Option<String>,
    label: String,
    base: String,
    key: String,
) -> GatewayViewV1 {
    dispatch(
        caller,
        expected_profile,
        GatewayCommand::Save,
        expected_connection_id,
        Some((label, base, key)),
    )
    .await
}
#[tauri::command]
pub(crate) async fn gateway_check(
    caller: WebviewWindow,
    expected_profile: String,
    expected_connection_id: String,
) -> GatewayViewV1 {
    dispatch(
        caller,
        expected_profile,
        GatewayCommand::Check,
        Some(expected_connection_id),
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn gateway_disconnect(
    caller: WebviewWindow,
    expected_profile: String,
    expected_connection_id: String,
) -> GatewayViewV1 {
    dispatch(
        caller,
        expected_profile,
        GatewayCommand::Disconnect,
        Some(expected_connection_id),
        None,
    )
    .await
}
