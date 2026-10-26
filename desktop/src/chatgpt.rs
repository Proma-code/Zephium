//! Windows-only, main-chrome account commands. No credentials or auth URLs cross IPC.
use super::*;
use zephium_ipc::chatgpt::{ChatGptCommand, ChatGptViewV1};

#[cfg(all(windows, feature = "work-product"))]
mod live {
    use super::*;
    use std::collections::HashMap;
    use zephium_chatgpt::service::{Fault, Service};
    use zephium_core::ids::ProfileId;
    use zephium_core::work::model::*;
    use zephium_ipc::chatgpt::{ChatGptFaultV1, ChatGptStateV1};

    #[cfg(feature = "windows-subscription-trial")]
    const VAULT_NAMESPACE: &str = "app.zephium.subscription-trial";
    #[cfg(not(feature = "windows-subscription-trial"))]
    const VAULT_NAMESPACE: &str = "app.zephium";
    const HOST_KEY: &str = "chatgpt.local.host_id.v1";
    static REGISTRY: OnceLock<Arc<Registry>> = OnceLock::new();

    struct Registry {
        app: tauri::AppHandle,
        services: Mutex<HashMap<ProfileId, Arc<Service>>>,
        foreground: std::sync::RwLock<Option<String>>,
        scope_epoch: std::sync::atomic::AtomicU64,
    }

    impl Registry {
        // Read identity and epoch under one foreground lock so an A→B→A
        // transition cannot revive a command awaiting the actor profile view.
        fn capture_scope(&self, expected: &str) -> Option<u64> {
            let current = self.foreground.read().ok()?;
            (current.as_deref() == Some(expected))
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
            if current.as_deref() != Some(expected)
                || self.scope_epoch.load(std::sync::atomic::Ordering::Acquire) != epoch
            {
                return None;
            }
            service.scope().ok()
        }
        fn get(&self, profile: ProfileId) -> Option<Arc<Service>> {
            self.services.lock().ok()?.get(&profile).cloned()
        }

        fn service(
            &self,
            profile: ProfileId,
            require_foreground: bool,
        ) -> Result<Arc<Service>, Fault> {
            use zephium_core::ports::store::Store;
            let mut services = self.services.lock().map_err(|_| Fault::Vault)?;
            if let Some(service) = services.get(&profile) {
                return Ok(service.clone());
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
            let host = match store
                .app_setting(HOST_KEY)
                .filter(|value| !value.is_empty())
            {
                Some(host) => host,
                None => {
                    let host = zephium_chatgpt::oauth::new_host_id().map_err(Fault::from)?;
                    if !store.set_app_setting(HOST_KEY.into(), host.clone()) {
                        return Err(Fault::Vault);
                    }
                    host
                }
            };
            let service = Arc::new(Service::new(
                profile.to_string(),
                host,
                Arc::new(
                    zephium_chatgpt::vault::SystemVault::new(VAULT_NAMESPACE)
                        .map_err(|_| Fault::Vault)?,
                ),
                Arc::new(|| zephium_app::work_models::chatgpt_session_changed()),
            )?);
            service.set_foreground(
                self.foreground.read().ok().is_some_and(|current| {
                    current.as_deref() == Some(profile.to_string().as_str())
                }),
            );
            services.insert(profile, service.clone());
            drop(services);
            Ok(service)
        }
    }

    struct AccountModelClient {
        service: Arc<Service>,
        model: String,
        lease: zephium_chatgpt::service::Lease,
        /// One Work run owns this client. A failed or dropped call is never
        /// replayed by model-driven helper reassignment within that run.
        terminal: tokio::sync::Mutex<Option<WorkModelError>>,
    }
    impl WorkModelClient for AccountModelClient {
        fn call<'a>(
            &'a self,
            request: WorkModelRequest,
            events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
        ) -> WorkModelFuture<'a> {
            Box::pin(async move {
                if request.model.provider != WorkModelProvider::OpenAi
                    || request.model.wire != WorkModelWire::OpenAiResponses
                    || request.model.model != self.model
                {
                    return Err(WorkModelError::ChatGpt {
                        fault: ChatGptModelFault::Unsupported,
                    });
                }
                let mut terminal = self.terminal.lock().await;
                if let Some(error) = *terminal {
                    return Err(error);
                }
                // Set before dispatch, not only on return: dropping this future
                // must not reopen the same run's client for a second request.
                *terminal = Some(WorkModelError::ChatGpt {
                    fault: ChatGptModelFault::Unavailable,
                });
                let result = self.service.call(self.lease, request, events).await;
                *terminal = match &result {
                    Ok(_) => None,
                    Err(error @ WorkModelError::ChatGpt { .. }) => Some(*error),
                    Err(_) => Some(WorkModelError::ChatGpt {
                        fault: ChatGptModelFault::Unavailable,
                    }),
                };
                result
            })
        }
    }

    impl zephium_app::work_models::WorkChatGptSession for Registry {
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
                .map(|service| service.models().iter().map(|model| model.entry()).collect())
                .unwrap_or_default()
        }
        fn client(
            &self,
            profile: ProfileId,
            model: &str,
        ) -> Result<Arc<dyn WorkModelClient>, WorkModelError> {
            let service = self.get(profile).ok_or(WorkModelError::ChatGpt {
                fault: ChatGptModelFault::SignedOut,
            })?;
            if !service.models().iter().any(|entry| entry.slug == model) {
                return Err(WorkModelError::ChatGpt {
                    fault: ChatGptModelFault::SignedOut,
                });
            }
            let lease = service.lease().ok_or(WorkModelError::ChatGpt {
                fault: ChatGptModelFault::SignedOut,
            })?;
            Ok(Arc::new(AccountModelClient {
                service,
                model: model.into(),
                lease,
                terminal: tokio::sync::Mutex::new(None),
            }))
        }
    }

    pub(super) fn install(app: &tauri::AppHandle) {
        let registry = REGISTRY.get_or_init(|| {
            Arc::new(Registry {
                app: app.clone(),
                services: Mutex::new(HashMap::new()),
                foreground: std::sync::RwLock::new(None),
                scope_epoch: std::sync::atomic::AtomicU64::new(0),
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
            let services = registry
                .services
                .lock()
                .map(|services| services.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for service in services {
                service.close();
            }
        }
    }

    pub(super) async fn drained() -> bool {
        let services = REGISTRY
            .get()
            .and_then(|registry| {
                registry
                    .services
                    .lock()
                    .ok()
                    .map(|services| services.values().cloned().collect::<Vec<_>>())
            })
            .unwrap_or_default();
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
            Fault::Denied => ChatGptFaultV1::Denied,
            Fault::Cancelled => ChatGptFaultV1::Cancelled,
            Fault::Timeout => ChatGptFaultV1::Timeout,
            Fault::Protocol => ChatGptFaultV1::Protocol,
            Fault::Vault => ChatGptFaultV1::Vault,
            Fault::SessionExpired => ChatGptFaultV1::SessionExpired,
            Fault::UsageLimit => ChatGptFaultV1::UsageLimit,
            Fault::Busy => ChatGptFaultV1::Busy,
        }
    }

    fn view(profile: String, service: &Service, error: Option<Fault>) -> ChatGptViewV1 {
        let current = service.view();
        ChatGptViewV1 {
            version: 1,
            profile,
            state: if current.signing_in {
                ChatGptStateV1::SigningIn
            } else if current.connected {
                ChatGptStateV1::Connected
            } else {
                ChatGptStateV1::SignedOut
            },
            email: current.email,
            models: current.models.try_into().unwrap_or(0),
            login_attempt: current.login_attempt,
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

    async fn open_browser(
        app: tauri::AppHandle,
        service: std::sync::Weak<Service>,
        attempt: String,
        url: zeroize::Zeroizing<String>,
    ) -> bool {
        // The service supplies only its constructed fixed-origin OAuth URL.
        if !url.starts_with("https://auth.openai.com/api/accounts/authorize?") {
            return false;
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        let app_for_open = app.clone();
        if app
            .run_on_main_thread(move || {
                use windows::core::{w, HSTRING, PCWSTR};
                use windows::Win32::UI::Shell::ShellExecuteW;
                use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
                if shutdown_started(&app_for_open)
                    || service.upgrade().is_none_or(|service| {
                        service.view().login_attempt.as_deref() != Some(attempt.as_str())
                    })
                {
                    let _ = send.send(false);
                    return;
                }
                let target = HSTRING::from(url.as_str());
                // SAFETY: an exact HTTPS authorization URL is held for the call;
                // no executable, arbitrary scheme, shell command or arguments are accepted.
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
                let _ = send.send(result.0 as usize > 32);
            })
            .is_err()
        {
            return false;
        }
        receive.await.unwrap_or(false)
    }

    pub(super) async fn dispatch(
        caller: WebviewWindow,
        expected: String,
        action: ChatGptCommand,
        attempt: Option<String>,
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
            Ok(service) => service,
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
            ChatGptCommand::SignIn => {
                match attempt.filter(|id| zephium_chatgpt::service::valid_attempt(id)) {
                    Some(attempt) => {
                        let app = registry.app.clone();
                        let weak = Arc::downgrade(&service);
                        let expected = attempt.clone();
                        service
                            .sign_in_scoped(service_scope, attempt, move |url| {
                                open_browser(app, weak, expected, url)
                            })
                            .await
                    }
                    None => Err(Fault::Invalid),
                }
            }
            ChatGptCommand::Cancel => {
                match attempt.filter(|id| zephium_chatgpt::service::valid_attempt(id)) {
                    Some(attempt) => service.cancel_scoped(service_scope, &attempt).await,
                    None => Err(Fault::Invalid),
                }
            }
            ChatGptCommand::SignOut => service.sign_out_scoped(service_scope).await,
            ChatGptCommand::RefreshModels => service.refresh_models_scoped(service_scope).await,
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
    attempt: Option<String>,
) -> ChatGptViewV1 {
    #[cfg(all(windows, feature = "work-product"))]
    {
        live::dispatch(caller, expected, action, attempt).await
    }
    #[cfg(not(all(windows, feature = "work-product")))]
    {
        let _ = (caller, action, attempt);
        ChatGptViewV1::unavailable(expected)
    }
}

#[tauri::command]
pub(crate) async fn chatgpt_account(
    caller: WebviewWindow,
    expected_profile: String,
) -> ChatGptViewV1 {
    dispatch(caller, expected_profile, ChatGptCommand::Account, None).await
}
#[tauri::command]
pub(crate) async fn chatgpt_sign_in(
    caller: WebviewWindow,
    expected_profile: String,
    attempt: String,
) -> ChatGptViewV1 {
    dispatch(
        caller,
        expected_profile,
        ChatGptCommand::SignIn,
        Some(attempt),
    )
    .await
}
#[tauri::command]
pub(crate) async fn chatgpt_cancel(
    caller: WebviewWindow,
    expected_profile: String,
    attempt: String,
) -> ChatGptViewV1 {
    dispatch(
        caller,
        expected_profile,
        ChatGptCommand::Cancel,
        Some(attempt),
    )
    .await
}
#[tauri::command]
pub(crate) async fn chatgpt_sign_out(
    caller: WebviewWindow,
    expected_profile: String,
) -> ChatGptViewV1 {
    dispatch(caller, expected_profile, ChatGptCommand::SignOut, None).await
}
#[tauri::command]
pub(crate) async fn chatgpt_refresh_models(
    caller: WebviewWindow,
    expected_profile: String,
) -> ChatGptViewV1 {
    dispatch(
        caller,
        expected_profile,
        ChatGptCommand::RefreshModels,
        None,
    )
    .await
}
