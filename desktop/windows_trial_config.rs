//! Build-time isolation contract for the Windows-only side-by-side package.

use serde_json::Value;

// Exercise the same pure name selection used by the native Windows adapter,
// without loading Tauri/WebView2 or changing a real registry value.
#[cfg(test)]
#[path = "windows_startup_identity.rs"]
mod windows_startup_identity;

pub const IDENTIFIER: &str = "app.zephium.subscription-trial";
pub const PRODUCT: &str = "Zephium Subscription Trial";

pub fn validate(config: &Value, target: &str, enabled: bool) -> Result<(), &'static str> {
    let trial_identity = config.get("identifier").and_then(Value::as_str) == Some(IDENTIFIER);
    if !enabled && !trial_identity {
        return Ok(());
    }
    if !enabled || !trial_identity || target != "windows" {
        return Err(
            "the subscription trial requires its Windows feature and exact isolated identity",
        );
    }
    for (pointer, expected) in [
        ("/productName", PRODUCT),
        ("/mainBinaryName", "zephium-subscription-trial"),
        ("/app/windows/0/title", PRODUCT),
        ("/bundle/windows/nsis/installMode", "currentUser"),
        ("/bundle/windows/nsis/startMenuFolder", PRODUCT),
        (
            "/bundle/windows/nsis/installerHooks",
            "windows/subscription-trial-hooks.nsh",
        ),
        ("/bundle/windows/webviewInstallMode/type", "skip"),
    ] {
        if config.pointer(pointer).and_then(Value::as_str) != Some(expected) {
            return Err("the subscription trial package identity or installer isolation changed");
        }
    }
    if config
        .pointer("/build/devUrl")
        .is_some_and(|value| !value.is_null())
        || config
            .pointer("/bundle/createUpdaterArtifacts")
            .and_then(Value::as_bool)
            != Some(false)
        || config.pointer("/bundle/targets") != Some(&serde_json::json!(["nsis"]))
        || config.pointer("/bundle/fileAssociations") != Some(&serde_json::json!([]))
        || config
            .pointer("/plugins/updater")
            .is_some_and(|value| !value.is_null())
        || config
            .pointer("/plugins/deep-link")
            .is_some_and(|value| !value.is_null())
    {
        return Err(
            "the subscription trial cannot enable an updater, associations, or development URL",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial() -> Value {
        serde_json::from_str(include_str!("tauri.windows-subscription-trial.conf.json")).unwrap()
    }

    #[test]
    fn exact_windows_trial_is_accepted() {
        assert!(validate(&trial(), "windows", true).is_ok());
    }

    #[test]
    fn feature_identity_and_target_are_inseparable() {
        assert!(validate(&trial(), "windows", false).is_err());
        assert!(validate(&trial(), "linux", true).is_err());
        assert!(validate(&trial(), "macos", true).is_err());
        let product = serde_json::from_str(include_str!("tauri.conf.json")).unwrap();
        assert!(validate(&product, "windows", true).is_err());
        assert!(validate(&product, "windows", false).is_ok());
    }

    #[test]
    fn cannot_reuse_product_names_or_installer_hook() {
        for (pointer, replacement) in [
            ("/identifier", "app.zephium"),
            ("/productName", "Zephium"),
            ("/mainBinaryName", "zephium-desktop"),
            ("/app/windows/0/title", "Zephium"),
            (
                "/bundle/windows/nsis/installerHooks",
                "windows/installer-hooks.nsh",
            ),
            ("/bundle/windows/nsis/installMode", "perMachine"),
            ("/bundle/windows/nsis/startMenuFolder", "Zephium"),
            (
                "/bundle/windows/webviewInstallMode/type",
                "downloadBootstrapper",
            ),
        ] {
            let mut config = trial();
            *config.pointer_mut(pointer).unwrap() = replacement.into();
            assert!(validate(&config, "windows", true).is_err(), "{pointer}");
        }
    }

    #[test]
    fn cannot_enable_updater_associations_or_unbundled_ui() {
        for (pointer, replacement) in [
            (
                "/plugins/updater",
                serde_json::json!({"endpoints": ["https://example.invalid/latest.json"]}),
            ),
            (
                "/plugins/deep-link",
                serde_json::json!({"desktop": {"schemes": ["https"]}}),
            ),
            ("/bundle/createUpdaterArtifacts", true.into()),
            (
                "/bundle/fileAssociations",
                serde_json::json!([{"ext": ["html"]}]),
            ),
            ("/bundle/targets", "all".into()),
            ("/build/devUrl", "http://localhost:1420".into()),
        ] {
            let mut config = trial();
            *config.pointer_mut(pointer).unwrap() = replacement;
            assert!(validate(&config, "windows", true).is_err(), "{pointer}");
        }
    }
}
