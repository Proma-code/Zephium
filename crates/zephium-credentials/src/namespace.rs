//! Final Windows vault boundary: old logical names never reach product items
//! from the side-by-side trial. Enumeration returns the caller's logical names.

use std::borrow::Cow;

const PRODUCT: &str = "app.zephium.";
const TRIAL: &str = "app.zephium.subscription-trial.";

pub(super) fn native(target: &str) -> Cow<'_, str> {
    if cfg!(feature = "windows-subscription-trial") && !target.starts_with(TRIAL) {
        if let Some(suffix) = target.strip_prefix(PRODUCT) {
            return Cow::Owned(format!("{TRIAL}{suffix}"));
        }
    }
    Cow::Borrowed(target)
}

pub(super) fn logical<'a>(target: &'a str, requested: &str) -> Cow<'a, str> {
    if cfg!(feature = "windows-subscription-trial") && !requested.starts_with(TRIAL) {
        if let Some(suffix) = target.strip_prefix(TRIAL) {
            return Cow::Owned(format!("{PRODUCT}{suffix}"));
        }
    }
    Cow::Borrowed(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_legacy_owners_and_cleanup_targets_are_isolated() {
        for target in [
            "app.zephium.account",
            "app.zephium.account|https://api.zephium.app",
            "app.zephium.agent-provider.openai",
            "app.zephium.agent-provider.typesafe",
            "app.zephium.connection.70726f66696c65.736572766572/oauth/",
            "app.zephium.chatgpt|profile|",
            "app.zephium.",
        ] {
            let actual = native(target);
            if cfg!(feature = "windows-subscription-trial") {
                assert!(actual.starts_with(TRIAL));
                assert_ne!(actual, target);
            } else {
                assert_eq!(actual, target);
            }
            assert_eq!(logical(&actual, target), target);
        }
    }

    #[test]
    fn explicit_trial_targets_are_idempotent_including_list_cleanup() {
        let prefix = "app.zephium.subscription-trial.chatgpt|profile|";
        let item = format!("{prefix}manifest");
        assert_eq!(native(prefix), prefix);
        assert_eq!(native(&item), item);
        assert_eq!(logical(&item, prefix), item);
    }

    #[test]
    fn lookalikes_do_not_escape_or_become_another_apps_target() {
        for target in ["other.app.key", "app.zephium", "app.zephium-other.key"] {
            assert_eq!(native(target), target);
        }
        let target = "app.zephium.subscription-trial-other.key";
        if cfg!(feature = "windows-subscription-trial") {
            assert_eq!(
                native(target),
                "app.zephium.subscription-trial.subscription-trial-other.key"
            );
        }
    }
}
