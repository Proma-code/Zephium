//! Windows-only, main-chrome account commands. No credentials or auth URLs cross IPC.
use super::*;
use zephium_ipc::chatgpt::{ChatGptCommand, ChatGptViewV1};

#[cfg(all(windows, feature = "work-product"))]
mod live {
    use super::*;
    use std::collections::HashMap;
    use zephium_chatgpt::service::{Fault, Presence, Service};
    use zephium_core::ids::ProfileId;
    use zephium_ipc::chatgpt::{ChatGptFaultV1, ChatGptPresenceV1, ChatGptStateV1};

    #[cfg(feature = "windows-subscription-trial")]
    const VAULT_NAMESPACE: &str = "app.zephium.subscription-trial";
    #[cfg(not(feature = "windows-subscription-trial"))]
    const VAULT_NAMESPACE: &str = "app.zephium";
    const HOST_KEY: &str = "chatgpt.local.host_id.v1";
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
        ) -> Option<zephium_chatgpt::service::Scope> {
            let current = self.foreground.read().ok()?;
            if self.closed.load(std::sync::atomic::Ordering::Acquire)
                || current.as_deref() != Some(expected)
                || self.scope_epoch.load(std::sync::atomic::Ordering::Acquire) != epoch
            {
                return None;
            }
            service.scope().ok()
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
        ) -> Result<Option<Arc<Service>>, Fault> {
            use zephium_core::ports::store::Store;
            let mut services = self.services.lock().map_err(|_| Fault::Vault)?;
            if self.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(Fault::Cancelled);
            }
            if let Some(service) = services.get(&profile) {
                return Ok(Some(service.clone()));
            }
            if require_foreground
                && self
                    .foreground
                    .read()
                    .map_err(|_| Fault::Invalid)?
                    .as_deref()
                    != Some(profile.to_string().as_str())
            {
                return Err(Fault::Cancelled);
            }
            services.retain(|_, service| !service.closed_and_drained());
            if services.len() >= 64 {
                return Err(Fault::Busy);
            }
            let store = APP_STORE.get().ok_or(Fault::Vault)?;
            let Some(host) =
                zephium_chatgpt::service::existing_host(store.app_setting_readback(HOST_KEY))?
            else {
                return Ok(None);
            };
            let service = Arc::new(
                Service::new(
                    profile.to_string(),
                    host,
                    Arc::new(
                        zephium_chatgpt::vault::SystemVault::new(VAULT_NAMESPACE)
                            .map_err(|_| Fault::Vault)?,
                    ),
                    Arc::new(zephium_app::work_models::chatgpt_session_changed),
                )
                .map_err(|_| Fault::Vault)?,
            );
            service.set_foreground(
                self.foreground.read().ok().is_some_and(|current| {
                    current.as_deref() == Some(profile.to_string().as_str())
                }),
            );
            services.insert(profile, service.clone());
            drop(services);
            Ok(Some(service))
        }
    }

    impl zephium_app::work_models::WorkChatGptSession for Registry {
        fn erase_profile(&self, profile: ProfileId, deadline: std::time::Instant) -> bool {
            let service = match self.service(profile, false) {
                Ok(service) => service,
                Err(_) => return false,
            };
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tauri::async_runtime::block_on(async move {
                tokio::time::timeout(remaining, async move {
                    if let Some(service) = service {
                        service.erase().await
                    } else {
                        let vault = zephium_chatgpt::vault::SystemVault::new(VAULT_NAMESPACE)
                            .map_err(|_| Fault::Vault)?;
                        tokio::task::spawn_blocking(move || {
                            use zephium_chatgpt::vault::AccountVault;
                            vault.forget(&profile.to_string())
                        })
                        .await
                        .map_err(|_| Fault::Vault)?
                        .map_err(|_| Fault::Vault)
                    }
                })
                .await
                .is_ok_and(|result| result.is_ok())
            })
        }
    }

    pub(super) fn install(_app: &tauri::AppHandle) {
        let registry = REGISTRY.get_or_init(|| {
            Arc::new(Registry {
                services: Mutex::new(HashMap::new()),
                foreground: std::sync::RwLock::new(None),
                scope_epoch: std::sync::atomic::AtomicU64::new(0),
                closed: std::sync::atomic::AtomicBool::new(false),
            })
        });
        zephium_app::work_models::install_chatgpt_session(registry.clone());
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
        zephium_app::work_models::chatgpt_session_changed();
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

    fn fault(fault: Fault) -> ChatGptFaultV1 {
        match fault {
            Fault::Invalid => ChatGptFaultV1::Invalid,
            Fault::Offline => ChatGptFaultV1::Offline,
            Fault::Cancelled => ChatGptFaultV1::Cancelled,
            Fault::Timeout => ChatGptFaultV1::Timeout,
            Fault::Protocol => ChatGptFaultV1::Protocol,
            Fault::Vault => ChatGptFaultV1::Vault,
            Fault::Busy => ChatGptFaultV1::Busy,
        }
    }

    fn view(profile: String, service: &Service, error: Option<Fault>) -> ChatGptViewV1 {
        let current = service.view();
        ChatGptViewV1 {
            version: 1,
            profile,
            state: if current.connected {
                ChatGptStateV1::Connected
            } else {
                ChatGptStateV1::SignedOut
            },
            email: current.email,
            presence: match current.presence {
                Presence::Missing => ChatGptPresenceV1::Missing,
                Presence::Stored => ChatGptPresenceV1::Stored,
                Presence::Unavailable => ChatGptPresenceV1::Unavailable,
            },
            error: error.or(current.fault).map(fault),
            revocation_unconfirmed: current.revocation_unconfirmed,
        }
    }

    async fn allowed(
        caller: &WebviewWindow,
        expected: &str,
        action: ChatGptCommand,
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

    // A missing installation key is not proof that protected registration data
    // is absent. Probe the exact profile without inventing or adopting a host.
    async fn observe_unbound(profile: &str) -> ChatGptViewV1 {
        let mut view = ChatGptViewV1::unavailable(profile.to_owned());
        let Ok(vault) = zephium_chatgpt::vault::SystemVault::new(VAULT_NAMESPACE) else {
            return view;
        };
        let owner = profile.to_owned();
        let record = tokio::task::spawn_blocking(move || {
            use zephium_chatgpt::vault::AccountVault;
            vault.load(&owner)
        })
        .await;
        match record
            .map(zephium_chatgpt::service::unbound_presence)
            .unwrap_or(Presence::Unavailable)
        {
            Presence::Missing => {
                view.presence = ChatGptPresenceV1::Missing;
                view.state = ChatGptStateV1::SignedOut;
                view.error = None;
            }
            Presence::Stored => {
                view.presence = ChatGptPresenceV1::Stored;
                view.error = Some(ChatGptFaultV1::Vault);
            }
            Presence::Unavailable => view.error = Some(ChatGptFaultV1::Vault),
        }
        view
    }

    pub(super) async fn dispatch(
        caller: WebviewWindow,
        expected: String,
        action: ChatGptCommand,
    ) -> ChatGptViewV1 {
        let Some(registry) = REGISTRY.get() else {
            return ChatGptViewV1::unavailable(expected);
        };
        let Some(scope_epoch) = registry.capture_scope(&expected) else {
            return ChatGptViewV1::unavailable(expected);
        };
        let Some(profile) = allowed(&caller, &expected, action).await else {
            return ChatGptViewV1::unavailable(expected);
        };
        if !registry.owns_scope(&expected, scope_epoch) {
            return ChatGptViewV1::unavailable(expected);
        }
        let service = match registry.service(profile, true) {
            Ok(Some(service)) => service,
            Ok(None) => {
                let result = observe_unbound(&expected).await;
                if !registry.owns_scope(&expected, scope_epoch) {
                    return ChatGptViewV1::unavailable(expected);
                }
                return result;
            }
            Err(error) => {
                let mut view = ChatGptViewV1::unavailable(expected);
                view.error = Some(fault(error));
                return view;
            }
        };
        let Some(service_scope) = registry.admit_scope(&expected, scope_epoch, &service) else {
            return ChatGptViewV1::unavailable(expected);
        };
        let result = match action {
            ChatGptCommand::Account => service.load_scoped(service_scope).await,
            ChatGptCommand::SignOut => service.sign_out_scoped(service_scope).await,
        };
        if !registry.owns_scope(&expected, scope_epoch) {
            return ChatGptViewV1::unavailable(expected);
        }
        view(expected, &service, result.err())
    }
}

pub(crate) fn install(app: &tauri::AppHandle) {
    #[cfg(all(windows, feature = "work-product"))]
    live::install(app);
    #[cfg(not(all(windows, feature = "work-product")))]
    let _ = app;
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
    action: ChatGptCommand,
) -> ChatGptViewV1 {
    #[cfg(all(windows, feature = "work-product"))]
    {
        live::dispatch(caller, expected, action).await
    }
    #[cfg(not(all(windows, feature = "work-product")))]
    {
        let _ = (caller, action);
        ChatGptViewV1::unavailable(expected)
    }
}

#[tauri::command]
pub(crate) async fn chatgpt_account(
    caller: WebviewWindow,
    expected_profile: String,
) -> ChatGptViewV1 {
    dispatch(caller, expected_profile, ChatGptCommand::Account).await
}
#[tauri::command]
pub(crate) async fn chatgpt_sign_out(
    caller: WebviewWindow,
    expected_profile: String,
) -> ChatGptViewV1 {
    dispatch(caller, expected_profile, ChatGptCommand::SignOut).await
}
