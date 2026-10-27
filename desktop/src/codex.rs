//! Native Codex trial integration, separate from existing direct-SIWC records.
use super::*;
use zephium_ipc::codex::*;

#[cfg(all(
    windows,
    feature = "windows-subscription-trial",
    feature = "work-product"
))]
mod admission;
#[cfg(all(
    windows,
    feature = "windows-subscription-trial",
    feature = "work-product"
))]
mod delivery;
#[cfg(feature = "work-product")]
mod history;
#[cfg(all(
    windows,
    feature = "windows-subscription-trial",
    feature = "work-product"
))]
mod live;
#[cfg(all(
    windows,
    feature = "windows-subscription-trial",
    feature = "work-product"
))]
mod quota;

pub(crate) fn install(app: &tauri::AppHandle) {
    #[cfg(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    ))]
    live::install(app);
    #[cfg(not(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    )))]
    let _ = app;
}
pub(crate) fn profiles_changed(view: &zephium_ipc::ProfilesView) {
    #[cfg(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    ))]
    live::profiles_changed(view);
    #[cfg(not(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    )))]
    let _ = view;
}
pub(crate) fn stop() {
    #[cfg(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    ))]
    live::stop();
}
pub(crate) async fn drained() -> bool {
    #[cfg(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    ))]
    {
        live::drained().await
    }
    #[cfg(not(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    )))]
    {
        true
    }
}
async fn dispatch(
    caller: WebviewWindow,
    expected: String,
    command: CodexCommand,
    attempt: Option<String>,
    selection: Option<Option<CodexSelectionV1>>,
) -> CodexViewV1 {
    #[cfg(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    ))]
    {
        live::dispatch(caller, expected, command, attempt, selection).await
    }
    #[cfg(not(all(
        windows,
        feature = "windows-subscription-trial",
        feature = "work-product"
    )))]
    {
        let _ = (caller, command, attempt, selection);
        CodexViewV1::unavailable(expected)
    }
}
#[tauri::command]
pub(crate) async fn codex_account(caller: WebviewWindow, expected_profile: String) -> CodexViewV1 {
    dispatch(caller, expected_profile, CodexCommand::Account, None, None).await
}
#[tauri::command]
pub(crate) async fn codex_check_connection(
    caller: WebviewWindow,
    expected_profile: String,
) -> CodexViewV1 {
    dispatch(
        caller,
        expected_profile,
        CodexCommand::CheckConnection,
        None,
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn codex_connect(
    caller: WebviewWindow,
    expected_profile: String,
    attempt: String,
) -> CodexViewV1 {
    dispatch(
        caller,
        expected_profile,
        CodexCommand::Connect,
        Some(attempt),
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn codex_cancel_login(
    caller: WebviewWindow,
    expected_profile: String,
    attempt: String,
) -> CodexViewV1 {
    dispatch(
        caller,
        expected_profile,
        CodexCommand::CancelLogin,
        Some(attempt),
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn codex_logout(caller: WebviewWindow, expected_profile: String) -> CodexViewV1 {
    dispatch(caller, expected_profile, CodexCommand::Logout, None, None).await
}
#[tauri::command]
pub(crate) async fn codex_refresh_models(
    caller: WebviewWindow,
    expected_profile: String,
) -> CodexViewV1 {
    dispatch(
        caller,
        expected_profile,
        CodexCommand::RefreshModels,
        None,
        None,
    )
    .await
}

#[tauri::command]
pub(crate) async fn codex_refresh_quota(
    caller: WebviewWindow,
    expected_profile: String,
) -> CodexViewV1 {
    dispatch(
        caller,
        expected_profile,
        CodexCommand::RefreshQuota,
        None,
        None,
    )
    .await
}
#[tauri::command]
pub(crate) async fn codex_select_model(
    caller: WebviewWindow,
    expected_profile: String,
    selection: Option<CodexSelectionV1>,
) -> CodexViewV1 {
    dispatch(
        caller,
        expected_profile,
        CodexCommand::SelectModel,
        None,
        Some(selection),
    )
    .await
}

#[cfg(feature = "work-product")]
pub(crate) fn page_choice(
    profile: zephium_core::ids::ProfileId,
) -> Option<zephium_work_composition::page_assist::port::NativeConversationChoice> {
    #[cfg(all(windows, feature = "windows-subscription-trial"))]
    {
        live::page_choice(profile)
    }
    #[cfg(not(all(windows, feature = "windows-subscription-trial")))]
    {
        let _ = profile;
        None
    }
}
#[cfg(feature = "work-product")]
pub(crate) fn reserve_page(
    profile: zephium_core::ids::ProfileId,
    expected: zephium_work_composition::page_assist::port::NativeConversationChoice,
    draft: zephium_work_composition::page_assist::port::NativeConversationDraft,
) -> Result<
    Box<dyn zephium_work_composition::page_assist::port::NativeConversationReservation>,
    zephium_work_composition::page_assist::port::PageBackendError,
> {
    #[cfg(all(windows, feature = "windows-subscription-trial"))]
    {
        live::reserve_page(profile, expected, draft)
    }
    #[cfg(not(all(windows, feature = "windows-subscription-trial")))]
    {
        let _ = (profile, expected, draft);
        Err(
            zephium_work_composition::page_assist::port::PageBackendError::NativeConversation(
                zephium_ipc::page_assist::PageAssistNativeFaultV1::Unavailable,
            ),
        )
    }
}

#[cfg(feature = "work-product")]
pub(crate) async fn reserve_page_browser(
    profile: zephium_core::ids::ProfileId,
    expected: zephium_work_composition::page_assist::port::NativeConversationChoice,
    draft: zephium_work_composition::page_assist::port::NativeConversationDraft,
    manifest: zephium_work_composition::page_assist::port::NativeBrowserManifest,
) -> Result<
    Box<dyn zephium_work_composition::page_assist::port::NativeConversationReservation>,
    zephium_work_composition::page_assist::port::PageBackendError,
> {
    #[cfg(all(windows, feature = "windows-subscription-trial"))]
    {
        live::reserve_page_browser(profile, expected, draft, manifest).await
    }
    #[cfg(not(all(windows, feature = "windows-subscription-trial")))]
    {
        let _ = (profile, expected, draft, manifest);
        Err(
            zephium_work_composition::page_assist::port::PageBackendError::NativeConversation(
                zephium_ipc::page_assist::PageAssistNativeFaultV1::Unavailable,
            ),
        )
    }
}

#[cfg(feature = "work-product")]
pub(crate) fn page_requested(profile: zephium_core::ids::ProfileId) -> bool {
    #[cfg(all(windows, feature = "windows-subscription-trial"))]
    {
        live::page_requested(profile)
    }
    #[cfg(not(all(windows, feature = "windows-subscription-trial")))]
    {
        use zephium_core::{page_choice, ports::store::Store as _};
        !APP_STORE.get().is_some_and(|store| matches!(
            store.page_choice_call(profile, page_choice::Call::Read),
            page_choice::Reply::Loaded(snapshot) if snapshot.backend == page_choice::Backend::Model && snapshot.profile == profile && snapshot.valid()
        ))
    }
}
#[cfg(feature = "work-product")]
pub(crate) async fn commit_page_selection(
    profile: zephium_core::ids::ProfileId,
    expected: zephium_core::page_choice::Snapshot,
    selection: zephium_core::page_choice::Selection,
) -> Result<zephium_core::page_choice::Reply, CodexFaultV1> {
    #[cfg(all(windows, feature = "windows-subscription-trial"))]
    {
        live::commit_page_selection(profile, expected, selection).await
    }
    #[cfg(not(all(windows, feature = "windows-subscription-trial")))]
    {
        use zephium_core::{page_choice, ports::store::Store as _};
        if !matches!(selection, page_choice::Selection::Model(_)) {
            return Err(CodexFaultV1::Unavailable);
        }
        let store = APP_STORE.get().ok_or(CodexFaultV1::ProfileStorage)?;
        Ok(store.page_choice_call(
            profile,
            page_choice::Call::CompareAndSwap {
                expected: Box::new(expected),
                selection,
            },
        ))
    }
}

#[cfg(feature = "work-product")]
pub(crate) fn restore_history(
    body: String,
    association: Option<&zephium_core::codex_store::Association>,
) -> Option<String> {
    history::restore(body, association)
}
