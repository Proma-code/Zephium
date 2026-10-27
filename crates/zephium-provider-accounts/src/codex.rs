//! Managed ChatGPT authentication through the official Codex app-server protocol.
//!
//! This is a codec and reducer, not a process launcher or permission boundary.
//! A future qualified native broker must own one instance per connection, enforce
//! deadlines and user consent, and close that connection on protocol errors.
//! No credentials, account email, provider error text, or external-token API are
//! accepted or retained. Protocol: <https://learn.chatgpt.com/docs/app-server>.

use serde_json::{json, Value};
use url::Url;

/// Maximum size of one account-protocol message, before JSON decoding.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// A content-free view of the native CLI's observed authentication state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AccountState {
    /// No authoritative account read has completed on this connection.
    #[default]
    Unknown,
    /// An account read is pending.
    Checking,
    /// The CLI is creating its managed browser login.
    StartingLogin,
    /// The CLI returned a validated URL; the person must finish in the browser.
    AwaitingBrowser,
    /// Cancellation was requested but has not been acknowledged.
    Cancelling,
    /// The login ceremony was cancelled; this does not mean the CLI signed out.
    Cancelled,
    /// A fresh account read reported no account.
    SignedOut,
    /// A fresh account read reported ChatGPT authentication, not API credit.
    ChatGpt,
    /// A fresh account read reported an API key, not a ChatGPT subscription.
    ApiKey,
    /// A fresh account read reported a different supported authentication mode.
    Other,
    /// The connection cannot be used again after an error.
    Failed,
    /// The broker ended the connection; no account state is claimed.
    Closed,
}

/// Closed errors: never contain server text, login codes, or credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// Login or logout needs a fresh explicit user action.
    ConsentRequired,
    /// Another handshake, account read, or login ceremony is outstanding.
    Busy,
    /// The initialization handshake has not completed.
    NotInitialized,
    /// This connection has been closed or has failed.
    ConnectionClosed,
    /// A message exceeds the pre-decoding byte bound.
    MessageTooLarge,
    /// The native peer returned an invalid account-protocol message.
    Protocol,
    /// The native peer returned an error; its untrusted text was discarded.
    ProviderRefused,
    /// The login URL is not an exact allowlisted HTTPS origin.
    LoginUrl,
    /// A host-enforced deadline expired; account outcome remains unknown.
    TimedOut,
}

#[derive(Clone, Debug)]
enum Method {
    Initialize,
    Read,
    Login,
    Cancel(String),
    Logout,
    LogoutDeferredRead,
}

/// A narrowly typed request for an already-qualified native transport.
///
/// Constructed only by the session. It cannot carry arbitrary methods, URLs,
/// credentials, prompts, executable names, or shell arguments.
#[derive(Clone)]
pub struct Request {
    id: u64,
    method: Method,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Request")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Request {
    /// Process-local correlation identity, never reused by this session.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Serialize the official account request; callers supply framing only.
    pub fn to_json(&self) -> Value {
        match &self.method {
            Method::Initialize => json!({
                "id": self.id,
                "method": "initialize",
                "params": {"clientInfo": {
                    "name": "zephium", "title": "Zephium", "version": env!("CARGO_PKG_VERSION")
                }}
            }),
            Method::Read => {
                json!({"id": self.id, "method": "account/read", "params": {"refreshToken": false}})
            }
            Method::Login => {
                json!({"id": self.id, "method": "account/login/start", "params": {"type": "chatgpt"}})
            }
            Method::Cancel(login_id) => {
                json!({"id": self.id, "method": "account/login/cancel", "params": {"loginId": login_id}})
            }
            Method::Logout | Method::LogoutDeferredRead => {
                json!({"id": self.id, "method": "account/logout"})
            }
        }
    }
}

/// A transient browser destination returned by the official login ceremony.
///
/// Do not log, persist, or use this value as an account identifier. The URL can
/// contain OAuth state. Debug output deliberately omits it.
pub struct LoginUrl(String);

impl LoginUrl {
    /// The validated HTTPS URL to open only in response to the person's login.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for LoginUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LoginUrl([redacted])")
    }
}

/// What a message requires from the broker; no variant executes an effect.
#[derive(Debug)]
pub enum Event {
    /// Unrelated or stale messages do not change state.
    Ignored,
    /// The state changed without authorizing another external action.
    Changed,
    /// Initialization succeeded; send `initialized` before any next request.
    Initialized,
    /// Open the CLI-owned sign-in URL after the already-confirmed login action.
    OpenBrowser(LoginUrl),
    /// Send a follow-up read or cancellation through the same owned transport.
    Send(Request),
}

/// The required notification after a successful initialization response.
pub fn initialized_notification() -> Value {
    json!({"method": "initialized", "params": {}})
}

/// One single-flight managed-account session bound to one native connection.
#[derive(Default)]
pub struct CodexAccountSession {
    ready: bool,
    terminal: bool,
    next_id: u64,
    pending: Option<Request>,
    login_id: Option<String>,
    cancel_requested: bool,
    read_invalidated: bool,
    state: AccountState,
}

impl CodexAccountSession {
    /// Create a fresh uninitialized connection reducer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Content-free status; login completion alone never yields `ChatGpt`.
    pub fn state(&self) -> AccountState {
        self.state
    }

    fn request(&mut self, method: Method) -> Result<Request, Fault> {
        if matches!(method, Method::Read) {
            self.read_invalidated = false;
        }
        self.next_id = self.next_id.checked_add(1).ok_or(Fault::Protocol)?;
        let request = Request {
            id: self.next_id,
            method,
        };
        self.pending = Some(request.clone());
        Ok(request)
    }

    fn idle(&self) -> Result<(), Fault> {
        if self.terminal {
            return Err(Fault::ConnectionClosed);
        }
        if !self.ready {
            return Err(Fault::NotInitialized);
        }
        if self.pending.is_some() || self.login_id.is_some() {
            return Err(Fault::Busy);
        }
        Ok(())
    }

    /// Begin the stable handshake once, identifying Zephium rather than Codex UI.
    pub fn initialize(&mut self) -> Result<Request, Fault> {
        if self.terminal {
            return Err(Fault::ConnectionClosed);
        }
        if self.ready || self.pending.is_some() {
            return Err(Fault::Busy);
        }
        self.request(Method::Initialize)
    }

    /// Observe the CLI account without requesting token refresh or reading files.
    pub fn read_account(&mut self) -> Result<Request, Fault> {
        self.idle()?;
        self.state = AccountState::Checking;
        self.request(Method::Read)
    }

    /// Begin browser login after fresh action-time consent, never on discovery.
    ///
    /// The broker must independently enforce consent and confinement. This
    /// argument records its decision; it does not grant process authority.
    pub fn begin_login(&mut self, user_confirmed: bool) -> Result<Request, Fault> {
        if !user_confirmed {
            return Err(Fault::ConsentRequired);
        }
        self.idle()?;
        self.cancel_requested = false;
        self.state = AccountState::StartingLogin;
        self.request(Method::Login)
    }

    /// Cancel the pending ceremony, including before its start response arrives.
    ///
    /// `None` means cancellation waits for the start response's exact login ID.
    /// A late completion cannot mark this session signed in after cancellation.
    pub fn cancel_login(&mut self) -> Result<Option<Request>, Fault> {
        if self.terminal {
            return Err(Fault::ConnectionClosed);
        }
        if self.cancel_requested {
            return Err(Fault::Busy);
        }
        if let Some(login_id) = self.login_id.take() {
            self.cancel_requested = true;
            self.state = AccountState::Cancelling;
            return self.request(Method::Cancel(login_id)).map(Some);
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|request| matches!(request.method, Method::Login))
        {
            self.cancel_requested = true;
            self.state = AccountState::Cancelling;
            return Ok(None);
        }
        Err(Fault::Busy)
    }

    /// Nonmutating readiness only, not user consent or logout authority. The
    /// native removal owner checks this before publishing protected intent.
    pub fn logout_ready(&self) -> Result<(), Fault> {
        self.idle()
    }

    /// Request logout only after a separate explicit user decision.
    ///
    /// Logout affects the official CLI's account, not Zephium's API-key vault.
    pub fn logout(&mut self, user_confirmed: bool) -> Result<Request, Fault> {
        if !user_confirmed {
            return Err(Fault::ConsentRequired);
        }
        self.idle()?;
        self.state = AccountState::Checking;
        self.request(Method::Logout)
    }

    /// Explicit logout with confirmation read creation deferred to its native
    /// owner. Used only when that owner must first observe the original logout
    /// notification. The ACK is not signed-out evidence; no freshness flag is
    /// cleared and every later read uses the ordinary invalidation rules.
    pub fn logout_deferred_read(&mut self, user_confirmed: bool) -> Result<Request, Fault> {
        if !user_confirmed {
            return Err(Fault::ConsentRequired);
        }
        self.idle()?;
        self.state = AccountState::Checking;
        self.request(Method::LogoutDeferredRead)
    }

    /// End this observer without claiming cancellation, logout, or success.
    pub fn close(&mut self) {
        self.terminal = true;
        self.pending = None;
        self.login_id = None;
        self.state = AccountState::Closed;
    }

    /// Record the broker's deadline; it must also terminate its owned transport.
    pub fn timeout(&mut self) -> Fault {
        self.fail(Fault::TimedOut)
    }

    fn fail(&mut self, fault: Fault) -> Fault {
        self.terminal = true;
        self.pending = None;
        self.login_id = None;
        self.state = AccountState::Failed;
        fault
    }

    /// Decode one bounded message from this connection's owned native peer.
    ///
    /// Errors are terminal and contain no server text. The host must not route
    /// messages from old processes into a replacement session.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<Event, Fault> {
        if self.terminal {
            return Err(Fault::ConnectionClosed);
        }
        let result = self.receive_inner(bytes);
        result.map_err(|fault| self.fail(fault))
    }

    fn receive_inner(&mut self, bytes: &[u8]) -> Result<Event, Fault> {
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(Fault::MessageTooLarge);
        }
        let message: Value = serde_json::from_slice(bytes).map_err(|_| Fault::Protocol)?;
        if !message.is_object() {
            return Err(Fault::Protocol);
        }
        if let Some(method) = message.get("method") {
            // Server requests cannot masquerade as account notifications.
            if message.get("id").is_some() {
                return Ok(Event::Ignored);
            }
            return self.notification(method.as_str().ok_or(Fault::Protocol)?, &message["params"]);
        }
        let id = message
            .get("id")
            .and_then(Value::as_u64)
            .ok_or(Fault::Protocol)?;
        if self.pending.as_ref().is_none_or(|pending| pending.id != id) {
            return Ok(Event::Ignored);
        }
        if message.get("error").is_some() {
            return Err(Fault::ProviderRefused);
        }
        let result = message
            .get("result")
            .filter(|result| result.is_object())
            .ok_or(Fault::Protocol)?;
        let pending = self.pending.take().ok_or(Fault::Protocol)?;
        match pending.method {
            Method::Initialize => {
                if result.get("userAgent").and_then(Value::as_str).is_none() {
                    return Err(Fault::Protocol);
                }
                self.ready = true;
                Ok(Event::Initialized)
            }
            Method::Read => {
                if self.read_invalidated {
                    return self.request(Method::Read).map(Event::Send);
                }
                if result
                    .get("requiresOpenaiAuth")
                    .and_then(Value::as_bool)
                    .is_none()
                {
                    return Err(Fault::Protocol);
                }
                let account = result.get("account").ok_or(Fault::Protocol)?;
                self.state = if account.is_null() {
                    AccountState::SignedOut
                } else {
                    match account.get("type").and_then(Value::as_str) {
                        Some("chatgpt") => AccountState::ChatGpt,
                        Some("apiKey") => AccountState::ApiKey,
                        Some("amazonBedrock") => AccountState::Other,
                        _ => return Err(Fault::Protocol),
                    }
                };
                Ok(Event::Changed)
            }
            Method::Login => {
                if result.get("type").and_then(Value::as_str) != Some("chatgpt") {
                    return Err(Fault::Protocol);
                }
                let id = result
                    .get("loginId")
                    .and_then(Value::as_str)
                    .filter(|id| valid_id(id))
                    .ok_or(Fault::Protocol)?
                    .to_owned();
                let url = login_url(
                    result
                        .get("authUrl")
                        .and_then(Value::as_str)
                        .ok_or(Fault::Protocol)?,
                )?;
                if self.cancel_requested {
                    return self.request(Method::Cancel(id)).map(Event::Send);
                }
                self.login_id = Some(id);
                self.state = AccountState::AwaitingBrowser;
                Ok(Event::OpenBrowser(url))
            }
            Method::Cancel(_) => {
                match result.get("status").and_then(Value::as_str) {
                    Some("canceled") => self.state = AccountState::Cancelled,
                    // A completed or unknown ceremony is not proof of cancellation.
                    Some("notFound") => self.state = AccountState::Unknown,
                    _ => return Err(Fault::Protocol),
                }
                self.cancel_requested = false;
                Ok(Event::Changed)
            }
            Method::Logout => {
                // Confirm through account/read; an acknowledgement is not account state.
                self.state = AccountState::Checking;
                self.request(Method::Read).map(Event::Send)
            }
            Method::LogoutDeferredRead => {
                // The native owner creates the one new confirmation read after
                // its separately validated notification barrier, never via a
                // fake result or by resetting an in-flight read's freshness.
                self.state = AccountState::Checking;
                Ok(Event::Changed)
            }
        }
    }

    fn notification(&mut self, method: &str, params: &Value) -> Result<Event, Fault> {
        match method {
            "account/login/completed" => {
                let Some(expected) = self.login_id.as_deref() else {
                    return Ok(Event::Ignored);
                };
                if params.get("loginId").and_then(Value::as_str) != Some(expected) {
                    return Ok(Event::Ignored);
                }
                let success = params
                    .get("success")
                    .and_then(Value::as_bool)
                    .ok_or(Fault::Protocol)?;
                self.login_id = None;
                if !success {
                    return Err(Fault::ProviderRefused);
                }
                self.state = AccountState::Checking;
                self.request(Method::Read).map(Event::Send)
            }
            "account/updated" => {
                // This notification has no login ID or request revision. It can
                // invalidate a prior observation, but cannot certify sign-in.
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|request| matches!(request.method, Method::Read))
                {
                    self.read_invalidated = true;
                    Ok(Event::Ignored)
                } else if self.login_id.is_none() && self.pending.is_none() {
                    self.state = AccountState::Unknown;
                    Ok(Event::Changed)
                } else {
                    Ok(Event::Ignored)
                }
            }
            _ => Ok(Event::Ignored),
        }
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn login_url(value: &str) -> Result<LoginUrl, Fault> {
    if value.len() > 8192
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || value.trim() != value
    {
        return Err(Fault::LoginUrl);
    }
    let url = Url::parse(value).map_err(|_| Fault::LoginUrl)?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("chatgpt.com" | "auth.openai.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.fragment().is_some()
    {
        return Err(Fault::LoginUrl);
    }
    Ok(LoginUrl(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive(session: &mut CodexAccountSession, message: Value) -> Result<Event, Fault> {
        session.receive(message.to_string().as_bytes())
    }

    fn ready() -> CodexAccountSession {
        let mut session = CodexAccountSession::new();
        let request = session.initialize().expect("initialize");
        assert_eq!(request.to_json()["params"]["clientInfo"]["name"], "zephium");
        assert!(request.to_json()["params"].get("capabilities").is_none());
        assert!(matches!(
            receive(
                &mut session,
                json!({"id": request.id(), "result": {"userAgent": "native"}})
            ),
            Ok(Event::Initialized)
        ));
        assert_eq!(
            initialized_notification(),
            json!({"method": "initialized", "params": {}})
        );
        session
    }

    fn browser(session: &mut CodexAccountSession, login_id: &str) {
        let request = session.begin_login(true).expect("login");
        assert_eq!(request.to_json()["params"], json!({"type": "chatgpt"}));
        let event = receive(session, json!({"id": request.id(), "result": {
            "type": "chatgpt", "loginId": login_id, "authUrl": "https://chatgpt.com/oauth/authorize?state=private"
        }})).expect("login response");
        let Event::OpenBrowser(url) = event else {
            panic!("browser expected")
        };
        assert_eq!(
            url.as_str(),
            "https://chatgpt.com/oauth/authorize?state=private"
        );
        assert!(!format!("{url:?}").contains("private"));
        assert_eq!(session.state(), AccountState::AwaitingBrowser);
    }

    fn completed(session: &mut CodexAccountSession, login_id: &str) -> Request {
        let Event::Send(request) = receive(session, json!({"method": "account/login/completed", "params": {"loginId": login_id, "success": true, "error": null}})).expect("completion") else { panic!("read expected") };
        assert_eq!(request.to_json()["method"], "account/read");
        assert_eq!(request.to_json()["params"]["refreshToken"], false);
        assert_eq!(session.state(), AccountState::Checking);
        request
    }

    #[test]
    fn handshake_and_each_mutation_require_their_own_admission() {
        let mut session = CodexAccountSession::new();
        assert_eq!(session.read_account().unwrap_err(), Fault::NotInitialized);
        assert_eq!(
            session.begin_login(false).unwrap_err(),
            Fault::ConsentRequired
        );
        session.initialize().expect("initialize");
        assert_eq!(session.initialize().unwrap_err(), Fault::Busy);

        let mut session = ready();
        assert_eq!(session.initialize().unwrap_err(), Fault::Busy);
        assert_eq!(session.logout(false).unwrap_err(), Fault::ConsentRequired);
        browser(&mut session, "attempt-1");
        assert_eq!(session.begin_login(true).unwrap_err(), Fault::Busy);
        assert_eq!(session.read_account().unwrap_err(), Fault::Busy);
        assert_eq!(session.logout(true).unwrap_err(), Fault::Busy);
    }

    #[test]
    fn completion_requires_a_fresh_matching_account_read() {
        let mut session = ready();
        browser(&mut session, "attempt-1");
        let read = completed(&mut session, "attempt-1");
        receive(&mut session, json!({"id": read.id(), "result": {"account": {"type": "chatgpt", "email": "private@example.com", "planType": "plus"}, "requiresOpenaiAuth": true}})).expect("account");
        assert_eq!(session.state(), AccountState::ChatGpt);
        // Neither email nor plan nor tokens are part of the public state.
        assert_eq!(format!("{:?}", session.state()), "ChatGpt");
    }

    #[test]
    fn api_keys_and_bedrock_are_not_chatgpt_subscription_accounts() {
        for (account, expected) in [
            (json!({"type": "apiKey"}), AccountState::ApiKey),
            (
                json!({"type": "amazonBedrock", "credentialSource": "awsManaged"}),
                AccountState::Other,
            ),
            (Value::Null, AccountState::SignedOut),
        ] {
            let mut session = ready();
            let read = session.read_account().expect("read");
            receive(&mut session, json!({"id": read.id(), "result": {"account": account, "requiresOpenaiAuth": true}})).expect("account");
            assert_eq!(session.state(), expected);
        }
    }

    #[test]
    fn stale_and_server_request_completions_cannot_change_login() {
        let mut session = ready();
        browser(&mut session, "attempt-2");
        assert!(matches!(
            receive(
                &mut session,
                json!({"method": "account/login/completed", "params": {"loginId": "attempt-1", "success": true}})
            ),
            Ok(Event::Ignored)
        ));
        assert!(matches!(
            receive(
                &mut session,
                json!({"id": 18, "method": "account/login/completed", "params": {"loginId": "attempt-2", "success": true}})
            ),
            Ok(Event::Ignored)
        ));
        assert!(matches!(
            receive(
                &mut session,
                json!({"method": "account/updated", "params": {"authMode": "chatgpt", "planType": "plus"}})
            ),
            Ok(Event::Ignored)
        ));
        assert_eq!(session.state(), AccountState::AwaitingBrowser);
    }

    #[test]
    fn cancellation_waits_for_acknowledgement_and_ignores_late_completion() {
        let mut session = ready();
        browser(&mut session, "attempt-1");
        let cancel = session.cancel_login().expect("cancel").expect("request");
        assert_eq!(cancel.to_json()["params"], json!({"loginId": "attempt-1"}));
        assert_eq!(session.state(), AccountState::Cancelling);
        assert_eq!(session.cancel_login().unwrap_err(), Fault::Busy);
        assert!(matches!(
            receive(
                &mut session,
                json!({"method": "account/login/completed", "params": {"loginId": "attempt-1", "success": true}})
            ),
            Ok(Event::Ignored)
        ));
        receive(
            &mut session,
            json!({"id": cancel.id(), "result": {"status": "canceled"}}),
        )
        .expect("cancelled");
        assert_eq!(session.state(), AccountState::Cancelled);
        browser(&mut session, "attempt-2");
        assert!(matches!(
            receive(
                &mut session,
                json!({"method": "account/login/completed", "params": {"loginId": "attempt-1", "success": true}})
            ),
            Ok(Event::Ignored)
        ));
        assert_eq!(session.state(), AccountState::AwaitingBrowser);
    }

    #[test]
    fn cancelling_before_start_response_never_opens_a_browser() {
        let mut session = ready();
        let login = session.begin_login(true).expect("login");
        assert!(session.cancel_login().expect("early cancel").is_none());
        let Event::Send(cancel) = receive(&mut session, json!({"id": login.id(), "result": {"type": "chatgpt", "loginId": "attempt-1", "authUrl": "https://auth.openai.com/authorize"}})).expect("start") else { panic!("cancel expected") };
        assert_eq!(cancel.to_json()["method"], "account/login/cancel");
        receive(
            &mut session,
            json!({"id": cancel.id(), "result": {"status": "notFound"}}),
        )
        .expect("cancel response");
        assert_eq!(session.state(), AccountState::Unknown);
    }

    #[test]
    fn newer_account_notifications_invalidate_inflight_reads() {
        let mut session = ready();
        let old = session.read_account().expect("read");
        receive(
            &mut session,
            json!({"method": "account/updated", "params": {"authMode": null}}),
        )
        .expect("updated");
        let Event::Send(new) = receive(&mut session, json!({"id": old.id(), "result": {"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true}})).expect("old read") else { panic!("fresh read expected") };
        assert_ne!(old.id(), new.id());
        assert_eq!(session.state(), AccountState::Checking);
        receive(
            &mut session,
            json!({"id": new.id(), "result": {"account": null, "requiresOpenaiAuth": true}}),
        )
        .expect("new read");
        assert_eq!(session.state(), AccountState::SignedOut);
        assert!(matches!(
            receive(
                &mut session,
                json!({"id": old.id(), "result": {"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true}})
            ),
            Ok(Event::Ignored)
        ));
        assert_eq!(session.state(), AccountState::SignedOut);
    }

    #[test]
    fn logout_is_confirmed_by_readback_not_acknowledgement() {
        let mut session = ready();
        let logout = session.logout(true).expect("logout");
        let Event::Send(read) =
            receive(&mut session, json!({"id": logout.id(), "result": {}})).expect("ack")
        else {
            panic!("read expected")
        };
        assert_eq!(session.state(), AccountState::Checking);
        receive(
            &mut session,
            json!({"id": read.id(), "result": {"account": null, "requiresOpenaiAuth": true}}),
        )
        .expect("read");
        assert_eq!(session.state(), AccountState::SignedOut);
    }

    #[test]
    fn login_url_rejects_lookalikes_credentials_localhost_and_parser_ambiguity() {
        for address in [
            "https://chatgpt.com.evil.example/login",
            "https://evilchatgpt.com/login",
            "http://chatgpt.com/login",
            "https://127.0.0.1/login",
            "file:///tmp/login",
            "https://user:secret@chatgpt.com/login",
            "https://chatgpt.com:8443/login",
            "https://chatgpt.com/login#fragment",
            "https://chatgpt.com\\@evil.example/",
            " https://chatgpt.com/login",
            "https://auth.openai.com/\nlogin",
        ] {
            assert_eq!(
                login_url(address).unwrap_err(),
                Fault::LoginUrl,
                "{address}"
            );
        }
        for address in [
            "https://chatgpt.com/authorize?state=test",
            "https://auth.openai.com/authorize",
            "https://chatgpt.com:443/login",
        ] {
            assert!(login_url(address).is_ok(), "{address}");
        }
    }

    #[test]
    fn malformed_oversized_and_wrong_login_types_fail_closed() {
        for bytes in [
            b"null".to_vec(),
            b"{".to_vec(),
            vec![b' '; MAX_MESSAGE_BYTES + 1],
        ] {
            let mut session = ready();
            assert!(session.receive(&bytes).is_err());
            assert_eq!(session.state(), AccountState::Failed);
            assert_eq!(session.read_account().unwrap_err(), Fault::ConnectionClosed);
        }
        for result in [
            json!({"type": "apiKey"}),
            json!({"type": "chatgptAuthTokens"}),
            json!({"type": "chatgpt", "loginId": "bad\n", "authUrl": "https://chatgpt.com/login"}),
        ] {
            let mut session = ready();
            let login = session.begin_login(true).expect("login");
            assert_eq!(
                receive(&mut session, json!({"id": login.id(), "result": result})).unwrap_err(),
                Fault::Protocol
            );
            assert_eq!(session.state(), AccountState::Failed);
        }
    }

    #[test]
    fn provider_errors_and_timeouts_never_leak_text_or_claim_success() {
        let mut session = ready();
        let login = session.begin_login(true).expect("login");
        let error = receive(&mut session, json!({"id": login.id(), "error": {"code": -1, "message": "secret-token private@example.com"}})).unwrap_err();
        assert_eq!(error, Fault::ProviderRefused);
        assert!(!format!("{error:?}").contains("secret"));
        let mut session = ready();
        browser(&mut session, "attempt-1");
        assert_eq!(session.timeout(), Fault::TimedOut);
        assert_eq!(session.state(), AccountState::Failed);
        session.close();
        assert_eq!(session.state(), AccountState::Closed);
        assert_eq!(receive(&mut session, json!({"method": "account/login/completed", "params": {"loginId": "attempt-1", "success": true}})).unwrap_err(), Fault::ConnectionClosed);
    }
}
