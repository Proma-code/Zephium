//! The per-user Windows startup value belongs to one application flavor.
//! Never probe, migrate or remove another flavor's Run/StartupApproved value.

pub(super) const fn login_item_name(subscription_trial: bool) -> &'static str {
    if subscription_trial {
        "Zephium Subscription Trial"
    } else {
        "Zephium"
    }
}

#[cfg(test)]
mod tests {
    use super::login_item_name;

    #[test]
    fn product_startup_value_name_remains_unchanged() {
        assert_eq!(login_item_name(false), "Zephium");
    }

    #[test]
    fn trial_startup_value_is_distinct_and_has_no_product_fallback() {
        assert_eq!(login_item_name(true), "Zephium Subscription Trial");
        assert_ne!(login_item_name(true), login_item_name(false));
    }
}
