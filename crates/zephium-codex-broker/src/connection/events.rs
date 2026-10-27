//! Data-only native observations. Historical facts survive later pump failures;
//! account/catalog/browser-launch authority does not.
use crate::{
    browser_owner::LocalOutcome as BrowserLocalOutcome,
    catalog::Catalog,
    conversation::{self, Scope},
    quota_owner::QuotaView,
};
#[cfg(windows)]
use zephium_core::codex_store as durable;
use zephium_provider_accounts::codex::{AccountState, LoginUrl};
/// A native Page conversation key, not a provider thread ID or resumed history.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ConversationKey(pub(crate) String);
impl ConversationKey {
    #[cfg(windows)]
    pub fn from_native(value: String) -> Option<Self> {
        durable::valid_conversation(&value).then_some(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
pub enum Event {
    Account(AccountState),
    OpenBrowser(LoginUrl),
    Catalog(Catalog),
    Quota(QuotaView),
    Conversation(conversation::Event),
    /// Original terminal is known, but durable continuation acknowledgement is not.
    HistoryUncertain(conversation::TurnResult),
    /// Original process/thread and the exact Page local-effect receipt both settled.
    BrowserSettled {
        reply: u64,
        local: BrowserLocalOutcome,
    },
    /// No model-frame handoff, exact durable Skip, and all preparation closed.
    NeverStarted {
        reply: u64,
    },
}
pub struct ScopedEvent {
    pub(crate) scope: Scope,
    pub(crate) conversation: Option<ConversationKey>,
    pub(crate) event: Event,
}
impl ScopedEvent {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn conversation(&self) -> Option<&ConversationKey> {
        self.conversation.as_ref()
    }
    pub fn into_event(self) -> Event {
        self.event
    }
}

pub(crate) fn preserve_failure(mut observed: Vec<ScopedEvent>, pending: &mut Vec<ScopedEvent>) {
    observed.retain(|event| {
        matches!(
            event.event,
            Event::Conversation(_)
                | Event::HistoryUncertain(_)
                | Event::NeverStarted { .. }
                | Event::BrowserSettled { .. }
        )
    });
    crate::wire::preserve_observed(observed, pending);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn historical_filter_preserves_no_send_and_uncertainty_but_not_authority() {
        use zephium_provider_accounts::codex::{CodexAccountSession, Event as AccountEvent};
        let scope = Scope::new("profile".into(), [1; 16], 1).unwrap();
        let key = ConversationKey("page".into());
        let scoped = |event| ScopedEvent {
            scope: scope.clone(),
            conversation: Some(key.clone()),
            event,
        };
        let mut account = CodexAccountSession::new();
        let init = account.initialize().unwrap();
        account
            .receive(
                json!({"id":init.id(),"result":{"userAgent":"fixture"}})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        let login = account.begin_login(true).unwrap();
        let AccountEvent::OpenBrowser(url) = account
            .receive(
                json!({"id":login.id(),"result":{
            "type":"chatgpt","loginId":"attempt","authUrl":"https://chatgpt.com/oauth/authorize"}})
                .to_string()
                .as_bytes(),
            )
            .unwrap()
        else {
            panic!()
        };
        let mut builder = crate::catalog::CatalogBuilder::new();
        builder
            .push_page(None, &json!({"data":[],"nextCursor":null}))
            .unwrap();
        let quota = crate::quota_owner::QuotaOwner::new(scope.clone(), 1).view();
        let authority = vec![
            scoped(Event::Account(AccountState::ChatGpt)),
            scoped(Event::Quota(quota)),
            scoped(Event::Catalog(builder.finish().unwrap())),
            scoped(Event::OpenBrowser(url)),
        ];
        for event in &authority {
            match &event.event {
                Event::Account(state) => assert_eq!(*state, AccountState::ChatGpt),
                Event::Quota(view) => assert_eq!(view.profile_revision, 1),
                Event::Catalog(catalog) => assert!(catalog.models().is_empty()),
                Event::OpenBrowser(url) => {
                    assert_eq!(url.as_str(), "https://chatgpt.com/oauth/authorize")
                }
                _ => panic!(),
            }
        }
        let result = conversation::TurnResult {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            outcome: conversation::Outcome::Completed,
            cancel_requested: false,
            messages: vec![],
            usage: None,
        };
        let mut observed = vec![
            scoped(Event::NeverStarted { reply: 1 }),
            scoped(Event::HistoryUncertain(result)),
        ];
        observed.extend(authority);
        let mut pending = Vec::new();
        preserve_failure(observed, &mut pending);
        assert_eq!(pending.len(), 2);
        assert!(matches!(
            pending.remove(0).into_event(),
            Event::NeverStarted { reply: 1 }
        ));
        assert!(
            matches!(pending.remove(0).into_event(), Event::HistoryUncertain(result) if result.outcome == conversation::Outcome::Completed)
        );
    }
}
