//! Scoped transport adapter for the existing provider-accounts Codex reducer.
//!
//! This module does not duplicate authentication state or perform I/O. The
//! native actor owns the one initialized transport, global wire-ID allocator,
//! current profile/action admission, deadlines, browser handoff and settlement.
//! Close and cancel are never logout. No API returns account email or tokens.

use serde_json::{json, Value};
use zephium_provider_accounts::codex::{
    AccountState, CodexAccountSession, Event as ReducerEvent, Fault as AccountFault, LoginUrl,
    Request as LogicalRequest, MAX_MESSAGE_BYTES,
};

use crate::conversation::Scope;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    ScopeChanged,
    WireId,
    State,
    Protocol,
    Account(AccountFault),
}

/// Native-only receipt. Construct only after the actor validates the actual
/// pinned initialize response, sends initialized, and admits this exact scope.
/// This constructor is not renderer consent or proof manufactured from JSON.
/// No second initialize request is emitted by the account adapter.
pub(crate) struct InitializedAccountReceipt {
    scope: Scope,
    wire_id: u64,
    result: Value,
}

impl InitializedAccountReceipt {
    pub(crate) fn from_native_handshake(
        scope: Scope,
        wire_id: u64,
        result: Value,
    ) -> Result<Self, Fault> {
        if wire_id == 0
            || !result.is_object()
            || !result
                .get("userAgent")
                .and_then(Value::as_str)
                .is_some_and(|agent| {
                    !agent.is_empty() && agent.len() <= 1024 && !agent.chars().any(char::is_control)
                })
        {
            return Err(Fault::Protocol);
        }
        bounded(&result)?;
        Ok(Self {
            scope,
            wire_id,
            result,
        })
    }
}

/// One native actor-issued, current-generation login action. No Clone,
/// deserialization, public constructor or renderer-controlled consent boolean.
pub(crate) struct LoginAction(Scope);
impl LoginAction {
    /// Only the native actor may issue after current profile/UI action checks.
    pub(crate) fn from_native_action(scope: Scope) -> Self {
        Self(scope)
    }
}

/// Logout requires a distinct action; login cancellation cannot mint this.
pub(crate) struct LogoutAction(Scope);
impl LogoutAction {
    /// Only the native actor may issue after a separate explicit logout action.
    pub(crate) fn from_native_action(scope: Scope) -> Self {
        Self(scope)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Notification {
    AccountUpdated,
    LoginCompleted,
}

pub(crate) enum Effect {
    Ignored,
    Changed(AccountState),
    RequestQueued,
    OpenBrowser(LoginUrl),
    /// An unsent login was discarded. The actor must close the connection;
    /// this is not proof of remote cancellation, logout, or signed-out state.
    CloseRequired,
}

/// Every effect retains native scope so a late browser/state event cannot be
/// projected into a replacement profile/connection without a matching fence.
pub(crate) struct AccountEvent {
    scope: Scope,
    effect: Effect,
    invalidates_admission: bool,
}
impl AccountEvent {
    pub(crate) fn scope(&self) -> &Scope {
        &self.scope
    }
    pub(crate) fn into_effect(self) -> Effect {
        self.effect
    }
    /// The actor must retire prepared text/model admission before another send.
    /// Even an uncorrelated account/updated invalidates; it never certifies login.
    pub(crate) fn invalidates_admission(&self) -> bool {
        self.invalidates_admission
    }
}

/// Move-only request. Debug intentionally reveals no login ID or payload.
pub(crate) struct AccountRequest {
    scope: Scope,
    wire_id: u64,
    message: Value,
}
impl AccountRequest {
    pub(crate) fn scope(&self) -> &Scope {
        &self.scope
    }
    pub(crate) fn wire_id(&self) -> u64 {
        self.wire_id
    }
    pub(crate) fn into_json(self) -> Value {
        self.message
    }
}
impl std::fmt::Debug for AccountRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountRequest")
            .field("wire_id", &self.wire_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RequestKind {
    Read,
    Login,
    Cancel,
    Logout,
}
struct Queued {
    request: LogicalRequest,
    kind: RequestKind,
}
struct Pending {
    wire_id: u64,
    logical_id: u64,
}

pub(crate) struct ManagedAccount {
    scope: Scope,
    reducer: CodexAccountSession,
    queued: Option<Queued>,
    pending: Option<Pending>,
    last_wire_id: u64,
}

impl ManagedAccount {
    pub(crate) fn from_initialized(receipt: InitializedAccountReceipt) -> Result<Self, Fault> {
        let mut reducer = CodexAccountSession::new();
        // Initialize the existing reducer's logical correlation only. This
        // request is never serialized to the native transport a second time.
        let logical = reducer.initialize().map_err(Fault::Account)?;
        let bytes = bounded(&json!({"id":logical.id(), "result":receipt.result}))?;
        if !matches!(
            reducer.receive(&bytes).map_err(Fault::Account)?,
            ReducerEvent::Initialized
        ) {
            return Err(Fault::Protocol);
        }
        Ok(Self {
            scope: receipt.scope,
            reducer,
            queued: None,
            pending: None,
            last_wire_id: receipt.wire_id,
        })
    }

    pub(crate) fn state(&self, scope: &Scope) -> Result<AccountState, Fault> {
        self.check_scope(scope)?;
        Ok(self.reducer.state())
    }

    pub(crate) fn read(&mut self, scope: &Scope) -> Result<AccountEvent, Fault> {
        self.check_scope(scope)?;
        let request = self.reducer.read_account().map_err(Fault::Account)?;
        self.queue(request, RequestKind::Read)
    }

    pub(crate) fn login(&mut self, action: LoginAction) -> Result<AccountEvent, Fault> {
        self.check_scope(&action.0)?;
        // The boolean is supplied only inside this adapter after consumption
        // of the separate native actor action, never passed by a renderer.
        let request = self.reducer.begin_login(true).map_err(Fault::Account)?;
        self.queue(request, RequestKind::Login)
    }

    pub(crate) fn logout(&mut self, action: LogoutAction) -> Result<AccountEvent, Fault> {
        self.check_scope(&action.0)?;
        let request = self.reducer.logout(true).map_err(Fault::Account)?;
        self.queue(request, RequestKind::Logout)
    }

    /// Connection wire-idleness alone does not drain a browser login ceremony.
    /// This exact reducer preflight must precede protected reservation writes.
    pub(crate) fn removal_ready(&self, scope: &Scope) -> Result<(), Fault> {
        self.check_scope(scope)?;
        if self.queued.is_some() || self.pending.is_some() {
            return Err(Fault::Account(AccountFault::Busy));
        }
        self.reducer.logout_ready().map_err(Fault::Account)
    }

    /// The held native removal transaction uses the same original logout wire
    /// request, but creates its fresh read only after the logout notification.
    pub(crate) fn logout_for_removal(
        &mut self,
        action: LogoutAction,
    ) -> Result<AccountEvent, Fault> {
        self.check_scope(&action.0)?;
        let request = self
            .reducer
            .logout_deferred_read(true)
            .map_err(Fault::Account)?;
        self.queue(request, RequestKind::Logout)
    }

    pub(crate) fn cancel_login(&mut self, scope: &Scope) -> Result<AccountEvent, Fault> {
        self.check_scope(scope)?;
        if self
            .queued
            .as_ref()
            .is_some_and(|queued| queued.kind == RequestKind::Login)
        {
            // No wire request was handed off. Never start authentication merely
            // to obtain an ID for cancellation after the person canceled.
            self.reducer.close();
            self.queued = None;
            self.pending = None;
            return Ok(self.event(Effect::CloseRequired));
        }
        match self.reducer.cancel_login().map_err(Fault::Account)? {
            Some(request) => self.queue(request, RequestKind::Cancel),
            None => Ok(self.event(Effect::Changed(self.reducer.state()))),
        }
    }

    /// `wire_id` comes only from the actor's monotonically increasing global
    /// allocator shared with all other RPC families on this connection.
    pub(crate) fn take_request(
        &mut self,
        scope: &Scope,
        wire_id: u64,
    ) -> Result<AccountRequest, Fault> {
        self.check_scope(scope)?;
        if matches!(
            self.reducer.state(),
            AccountState::Closed | AccountState::Failed
        ) {
            return Err(Fault::Account(AccountFault::ConnectionClosed));
        }
        if wire_id <= self.last_wire_id {
            return Err(Fault::WireId);
        }
        if self.pending.is_some() {
            return Err(Fault::State);
        }
        let queued = self.queued.take().ok_or(Fault::State)?;
        let logical_id = queued.request.id();
        let mut message = queued.request.to_json();
        message["id"] = json!(wire_id);
        if bounded(&message).is_err() {
            return self.fail(Fault::Protocol);
        }
        self.last_wire_id = wire_id;
        self.pending = Some(Pending {
            wire_id,
            logical_id,
        });
        Ok(AccountRequest {
            scope: self.scope.clone(),
            wire_id,
            message,
        })
    }

    /// The actor has already validated the envelope/source and routed this
    /// response by global wire ID. Provider errors are reduced to `Err(())`;
    /// their arbitrary body never enters account state or diagnostics.
    pub(crate) fn reply(
        &mut self,
        scope: &Scope,
        wire_id: u64,
        result: Result<Value, ()>,
    ) -> Result<AccountEvent, Fault> {
        self.check_scope(scope)?;
        let Some(pending) = self
            .pending
            .as_ref()
            .filter(|pending| pending.wire_id == wire_id)
        else {
            return Ok(self.event(Effect::Ignored));
        };
        let logical_id = pending.logical_id;
        let message = match result {
            Ok(result) => json!({"id":logical_id, "result":result}),
            Err(()) => json!({"id":logical_id, "error":{}}),
        };
        self.pending = None;
        self.receive(message)
    }

    /// Only these two account notification methods can reach the reducer.
    /// Parent transport rejects server requests masquerading as notifications.
    pub(crate) fn notification(
        &mut self,
        scope: &Scope,
        notification: Notification,
        params: Value,
    ) -> Result<AccountEvent, Fault> {
        self.check_scope(scope)?;
        let method = match notification {
            Notification::AccountUpdated => "account/updated",
            Notification::LoginCompleted => "account/login/completed",
        };
        let mut event = self.receive(json!({"method":method, "params":params}))?;
        if notification == Notification::AccountUpdated {
            event.invalidates_admission = true;
        }
        Ok(event)
    }

    pub(crate) fn close(&mut self, scope: &Scope) -> Result<AccountEvent, Fault> {
        self.check_scope(scope)?;
        self.reducer.close();
        self.queued = None;
        self.pending = None;
        Ok(self.event(Effect::Changed(AccountState::Closed)))
    }

    /// Logical timeout only. The actor must also settle its actual transport.
    pub(crate) fn timeout(&mut self, scope: &Scope) -> Result<AccountEvent, Fault> {
        self.check_scope(scope)?;
        self.queued = None;
        self.pending = None;
        Err(Fault::Account(self.reducer.timeout()))
    }

    fn check_scope(&self, scope: &Scope) -> Result<(), Fault> {
        if scope != &self.scope {
            return Err(Fault::ScopeChanged);
        }
        Ok(())
    }

    fn queue(&mut self, request: LogicalRequest, kind: RequestKind) -> Result<AccountEvent, Fault> {
        if self.queued.is_some() || self.pending.is_some() {
            return self.fail(Fault::State);
        }
        self.queued = Some(Queued { request, kind });
        Ok(self.event(Effect::RequestQueued))
    }

    fn receive(&mut self, message: Value) -> Result<AccountEvent, Fault> {
        let bytes = match bounded(&message) {
            Ok(bytes) => bytes,
            Err(fault) => return self.fail(fault),
        };
        let event = match self.reducer.receive(&bytes) {
            Ok(event) => event,
            Err(fault) => {
                self.queued = None;
                self.pending = None;
                return Err(Fault::Account(fault));
            }
        };
        match event {
            ReducerEvent::Ignored => Ok(self.event(Effect::Ignored)),
            ReducerEvent::Changed => Ok(self.event(Effect::Changed(self.reducer.state()))),
            ReducerEvent::OpenBrowser(url) => Ok(self.event(Effect::OpenBrowser(url))),
            ReducerEvent::Send(request) => {
                let kind = match request.to_json().get("method").and_then(Value::as_str) {
                    Some("account/read") => RequestKind::Read,
                    Some("account/login/cancel") => RequestKind::Cancel,
                    _ => return self.fail(Fault::Protocol),
                };
                self.queue(request, kind)
            }
            ReducerEvent::Initialized => self.fail(Fault::Protocol),
        }
    }

    fn event(&self, effect: Effect) -> AccountEvent {
        let invalidates_admission = !matches!(effect, Effect::Ignored);
        AccountEvent {
            scope: self.scope.clone(),
            effect,
            invalidates_admission,
        }
    }
    fn fail<T>(&mut self, fault: Fault) -> Result<T, Fault> {
        self.reducer.close();
        self.queued = None;
        self.pending = None;
        Err(fault)
    }
}

fn bounded(message: &Value) -> Result<Vec<u8>, Fault> {
    let bytes = serde_json::to_vec(message).map_err(|_| Fault::Protocol)?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Fault::Account(AccountFault::MessageTooLarge));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope::new("owned-profile".into(), [7; 16], 1).unwrap()
    }

    fn ready() -> ManagedAccount {
        let receipt = InitializedAccountReceipt::from_native_handshake(
            scope(),
            10,
            json!({"userAgent":"original-native"}),
        )
        .unwrap();
        ManagedAccount::from_initialized(receipt).unwrap()
    }

    fn request(session: &mut ManagedAccount, wire_id: u64, method: &str) -> Value {
        let request = session.take_request(&scope(), wire_id).unwrap();
        assert!(request.scope() == &scope());
        assert_eq!(request.wire_id(), wire_id);
        let value = request.into_json();
        assert_eq!(value["id"], wire_id);
        assert_eq!(value["method"], method);
        value
    }

    fn queued(event: AccountEvent) {
        assert!(event.scope() == &scope());
        assert!(event.invalidates_admission());
        assert!(matches!(event.into_effect(), Effect::RequestQueued));
    }

    fn login_result(id: &str) -> Value {
        json!({"type":"chatgpt", "loginId":id,
            "authUrl":"https://auth.openai.com/authorize?state=synthetic-private"})
    }

    fn observe_chatgpt(session: &mut ManagedAccount, wire_id: u64) {
        queued(session.read(&scope()).unwrap());
        request(session, wire_id, "account/read");
        let event = session
            .reply(
                &scope(),
                wire_id,
                Ok(json!({
                    "account":{"type":"chatgpt", "email":"synthetic@example.test"},
                    "requiresOpenaiAuth":true,
                })),
            )
            .unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::ChatGpt)
        ));
    }

    #[test]
    fn existing_handshake_is_consumed_without_a_second_wire_initialize() {
        let mut session = ready();
        assert_eq!(session.state(&scope()), Ok(AccountState::Unknown));
        assert!(matches!(
            session.take_request(&scope(), 11),
            Err(Fault::State)
        ));
        queued(session.read(&scope()).unwrap());
        assert!(matches!(
            session.take_request(&scope(), 10),
            Err(Fault::WireId)
        ));
        let message = request(&mut session, 40, "account/read");
        assert_eq!(message["params"], json!({"refreshToken":false}));
        assert!(matches!(
            session.read(&scope()),
            Err(Fault::Account(AccountFault::Busy))
        ));
        let stale = session
            .reply(
                &scope(),
                2,
                Ok(json!({"account":null,"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert!(matches!(stale.into_effect(), Effect::Ignored));
        assert_eq!(session.state(&scope()), Ok(AccountState::Checking));
        let event = session
            .reply(
                &scope(),
                40,
                Ok(json!({"account":null,"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::SignedOut)
        ));
        queued(session.read(&scope()).unwrap());
        assert!(matches!(
            session.take_request(&scope(), 40),
            Err(Fault::WireId)
        ));
        request(&mut session, 87, "account/read");
    }

    #[test]
    fn profile_connection_and_generation_mismatches_have_no_effect() {
        for stale in [
            Scope::new("other-profile".into(), [7; 16], 1).unwrap(),
            Scope::new("owned-profile".into(), [8; 16], 1).unwrap(),
            Scope::new("owned-profile".into(), [7; 16], 2).unwrap(),
        ] {
            let mut session = ready();
            assert_eq!(session.state(&stale), Err(Fault::ScopeChanged));
            assert!(matches!(session.read(&stale), Err(Fault::ScopeChanged)));
            assert!(matches!(
                session.login(LoginAction::from_native_action(stale.clone())),
                Err(Fault::ScopeChanged)
            ));
            assert!(matches!(
                session.logout(LogoutAction::from_native_action(stale.clone())),
                Err(Fault::ScopeChanged)
            ));
            assert!(matches!(
                session.cancel_login(&stale),
                Err(Fault::ScopeChanged)
            ));
            assert!(matches!(
                session.take_request(&stale, 99),
                Err(Fault::ScopeChanged)
            ));
            assert!(matches!(
                session.reply(&stale, 99, Ok(json!({}))),
                Err(Fault::ScopeChanged)
            ));
            assert!(matches!(
                session.notification(&stale, Notification::AccountUpdated, json!({})),
                Err(Fault::ScopeChanged)
            ));
            assert!(matches!(session.close(&stale), Err(Fault::ScopeChanged)));
            assert!(matches!(session.timeout(&stale), Err(Fault::ScopeChanged)));
            assert_eq!(session.state(&scope()), Ok(AccountState::Unknown));
        }
    }

    #[test]
    fn cancel_before_wire_handoff_never_starts_login_or_claims_logout() {
        let mut session = ready();
        queued(
            session
                .login(LoginAction::from_native_action(scope()))
                .unwrap(),
        );
        let event = session.cancel_login(&scope()).unwrap();
        assert!(event.invalidates_admission());
        assert!(matches!(event.into_effect(), Effect::CloseRequired));
        assert_eq!(session.state(&scope()), Ok(AccountState::Closed));
        assert!(matches!(
            session.take_request(&scope(), 11),
            Err(Fault::Account(AccountFault::ConnectionClosed))
        ));
        assert!(matches!(
            session.read(&scope()),
            Err(Fault::Account(AccountFault::ConnectionClosed))
        ));
    }

    #[test]
    fn cancel_after_handoff_waits_exact_login_id_and_never_opens_browser() {
        let mut session = ready();
        queued(
            session
                .login(LoginAction::from_native_action(scope()))
                .unwrap(),
        );
        assert_eq!(
            request(&mut session, 20, "account/login/start")["params"],
            json!({"type":"chatgpt"})
        );
        let event = session.cancel_login(&scope()).unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::Cancelling)
        ));
        queued(
            session
                .reply(&scope(), 20, Ok(login_result("actual-attempt-1")))
                .unwrap(),
        );
        let cancel = request(&mut session, 30, "account/login/cancel");
        assert_eq!(cancel["params"], json!({"loginId":"actual-attempt-1"}));
        let late = session
            .notification(
                &scope(),
                Notification::LoginCompleted,
                json!({"loginId":"actual-attempt-1", "success":true}),
            )
            .unwrap();
        assert!(matches!(late.into_effect(), Effect::Ignored));
        let event = session
            .reply(&scope(), 30, Ok(json!({"status":"canceled"})))
            .unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::Cancelled)
        ));
        assert_eq!(session.state(&scope()), Ok(AccountState::Cancelled));
        assert!(matches!(
            session.take_request(&scope(), 31),
            Err(Fault::State)
        ));
    }

    #[test]
    fn browser_url_is_transient_scoped_and_completion_requires_fresh_read() {
        let mut session = ready();
        queued(
            session
                .login(LoginAction::from_native_action(scope()))
                .unwrap(),
        );
        request(&mut session, 12, "account/login/start");
        let event = session
            .reply(&scope(), 12, Ok(login_result("matching-attempt")))
            .unwrap();
        assert!(event.scope() == &scope());
        let Effect::OpenBrowser(url) = event.into_effect() else {
            panic!("expected browser handoff")
        };
        assert!(url.as_str().starts_with("https://auth.openai.com/"));
        assert!(!format!("{url:?}").contains("synthetic-private"));
        assert_eq!(session.state(&scope()), Ok(AccountState::AwaitingBrowser));
        let stale = session
            .notification(
                &scope(),
                Notification::LoginCompleted,
                json!({"loginId":"stale-attempt", "success":true}),
            )
            .unwrap();
        assert!(matches!(stale.into_effect(), Effect::Ignored));
        queued(
            session
                .notification(
                    &scope(),
                    Notification::LoginCompleted,
                    json!({"loginId":"matching-attempt", "success":true}),
                )
                .unwrap(),
        );
        assert_eq!(session.state(&scope()), Ok(AccountState::Checking));
        request(&mut session, 50, "account/read");
        let event = session
            .reply(
                &scope(),
                50,
                Ok(json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::ChatGpt)
        ));
    }

    #[test]
    fn account_update_invalidates_admission_even_while_read_reply_is_pending() {
        let mut session = ready();
        queued(session.read(&scope()).unwrap());
        request(&mut session, 11, "account/read");
        let updated = session
            .notification(
                &scope(),
                Notification::AccountUpdated,
                json!({"authMode":"chatgpt"}),
            )
            .unwrap();
        assert!(updated.invalidates_admission());
        assert!(matches!(updated.into_effect(), Effect::Ignored));
        queued(
            session
                .reply(
                    &scope(),
                    11,
                    Ok(json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true})),
                )
                .unwrap(),
        );
        assert_eq!(session.state(&scope()), Ok(AccountState::Checking));
        request(&mut session, 90, "account/read");
        let event = session
            .reply(
                &scope(),
                90,
                Ok(json!({"account":null,"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert!(event.invalidates_admission());
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::SignedOut)
        ));
    }

    #[test]
    fn logout_needs_distinct_action_then_a_new_confirming_read() {
        let mut session = ready();
        observe_chatgpt(&mut session, 11);
        queued(
            session
                .logout(LogoutAction::from_native_action(scope()))
                .unwrap(),
        );
        request(&mut session, 70, "account/logout");
        queued(session.reply(&scope(), 70, Ok(json!({}))).unwrap());
        assert_eq!(session.state(&scope()), Ok(AccountState::Checking));
        request(&mut session, 101, "account/read");
        let event = session
            .reply(
                &scope(),
                101,
                Ok(json!({"account":null,"requiresOpenaiAuth":true})),
            )
            .unwrap();
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::SignedOut)
        ));
    }

    #[test]
    fn connection_close_and_timeout_never_enqueue_logout() {
        let mut session = ready();
        observe_chatgpt(&mut session, 11);
        let event = session.close(&scope()).unwrap();
        assert!(event.invalidates_admission());
        assert!(matches!(
            event.into_effect(),
            Effect::Changed(AccountState::Closed)
        ));
        assert!(matches!(
            session.take_request(&scope(), 12),
            Err(Fault::Account(AccountFault::ConnectionClosed))
        ));
        let mut timed = ready();
        queued(
            timed
                .login(LoginAction::from_native_action(scope()))
                .unwrap(),
        );
        assert!(matches!(
            timed.timeout(&scope()),
            Err(Fault::Account(AccountFault::TimedOut))
        ));
        assert_eq!(timed.state(&scope()), Ok(AccountState::Failed));
        assert!(matches!(
            timed.take_request(&scope(), 12),
            Err(Fault::Account(AccountFault::ConnectionClosed))
        ));
    }

    #[test]
    fn invalid_current_payload_and_provider_error_retire_mapping() {
        for result in [Ok(json!({"account":null})), Err(())] {
            let mut session = ready();
            queued(session.read(&scope()).unwrap());
            request(&mut session, 11, "account/read");
            assert!(matches!(
                session.reply(&scope(), 11, result),
                Err(Fault::Account(_))
            ));
            assert_eq!(session.state(&scope()), Ok(AccountState::Failed));
            assert!(matches!(
                session.take_request(&scope(), 12),
                Err(Fault::Account(AccountFault::ConnectionClosed))
            ));
        }
        let mut oversized = ready();
        queued(oversized.read(&scope()).unwrap());
        request(&mut oversized, 11, "account/read");
        assert!(matches!(
            oversized.reply(&scope(), 11, Ok(json!({"x":"x".repeat(MAX_MESSAGE_BYTES)}))),
            Err(Fault::Account(AccountFault::MessageTooLarge))
        ));
        assert_eq!(oversized.state(&scope()), Ok(AccountState::Closed));
    }

    #[test]
    fn initialize_receipt_shape_is_bounded_and_wire_zero_is_invalid() {
        for result in [
            json!(null),
            json!({}),
            json!({"userAgent":7}),
            json!({"userAgent":""}),
            json!({"userAgent":"line\nbreak"}),
        ] {
            assert!(InitializedAccountReceipt::from_native_handshake(scope(), 1, result).is_err());
        }
        assert!(InitializedAccountReceipt::from_native_handshake(
            scope(),
            0,
            json!({"userAgent":"original"})
        )
        .is_err());
        assert!(InitializedAccountReceipt::from_native_handshake(
            scope(),
            1,
            json!({"userAgent":"original", "large":"x".repeat(MAX_MESSAGE_BYTES)})
        )
        .is_err());
        let receipt = InitializedAccountReceipt::from_native_handshake(
            scope(),
            u64::MAX,
            json!({"userAgent":"original"}),
        )
        .unwrap();
        let mut session = ManagedAccount::from_initialized(receipt).unwrap();
        queued(session.read(&scope()).unwrap());
        assert!(matches!(
            session.take_request(&scope(), u64::MAX),
            Err(Fault::WireId)
        ));
    }
    #[test]
    fn removal_preflight_refuses_browser_login_without_disabling_its_cancel() {
        let mut session = ready();
        queued(
            session
                .login(LoginAction::from_native_action(scope()))
                .unwrap(),
        );
        request(&mut session, 11, "account/login/start");
        let event = session
            .reply(
                &scope(),
                11,
                Ok(json!({
                    "type":"chatgpt", "loginId":"synthetic-login",
                    "authUrl":"https://auth.openai.com/authorize?synthetic=1"
                })),
            )
            .unwrap();
        assert!(matches!(event.into_effect(), Effect::OpenBrowser(_)));
        assert_eq!(session.state(&scope()), Ok(AccountState::AwaitingBrowser));
        assert_eq!(
            session.removal_ready(&scope()),
            Err(Fault::Account(AccountFault::Busy))
        );
        assert_eq!(session.state(&scope()), Ok(AccountState::AwaitingBrowser));
        queued(session.cancel_login(&scope()).unwrap());
        assert_eq!(
            request(&mut session, 12, "account/login/cancel"),
            json!({"id":12,"method":"account/login/cancel","params":{"loginId":"synthetic-login"}})
        );
    }
}
