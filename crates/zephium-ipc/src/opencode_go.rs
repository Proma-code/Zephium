//! Secret-free OpenCode Go connection projection and explicit command vocabulary.
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GoStateV1 {
    Unconfigured,
    Configured,
    /// Public catalog observed; this does not verify a key or paid plan.
    Catalogued,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GoFaultV1 {
    Invalid,
    NotConfigured,
    Changed,
    Vault,
    Busy,
    Cancelled,
    Unauthorized,
    Forbidden,
    RateLimited,
    Unavailable,
    Timeout,
    Protocol,
    TooLarge,
    Incomplete,
    UsageUnavailable,
    MissingConversation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GoBillingSourceV1 {
    Free,
    Balance,
    Go,
    GoPlus,
    Byok,
}

/// Last completed request's reported source, not a balance or remaining allowance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct GoBillingViewV1 {
    pub source: GoBillingSourceV1,
    pub observed_at_ms: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GoQuotaStateV1 {
    NotChecked,
    Current,
    Stale,
    Unavailable,
    NoEntitlement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GoQuotaFaultV1 {
    Invalid,
    NotConfigured,
    Changed,
    Vault,
    Busy,
    Cancelled,
    Unauthorized,
    NoEntitlement,
    Forbidden,
    EndpointUnavailable,
    RateLimited,
    Timeout,
    Unavailable,
    Protocol,
    TooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GoQuotaKindV1 {
    Rolling,
    Weekly,
    Monthly,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct GoQuotaWindowV1 {
    pub kind: GoQuotaKindV1,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub resets_at_ms: String,
    pub limited: bool,
}

/// Explicit provider observation, separate from request billing and local usage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct GoQuotaViewV1 {
    pub state: GoQuotaStateV1,
    pub checking: bool,
    pub checked_at_ms: Option<String>,
    pub attempted_at_ms: Option<String>,
    pub stale_at_ms: Option<String>,
    pub windows: Vec<GoQuotaWindowV1>,
    pub error: Option<GoQuotaFaultV1>,
}
impl Default for GoQuotaViewV1 {
    fn default() -> Self {
        Self {
            state: GoQuotaStateV1::NotChecked,
            checking: false,
            checked_at_ms: None,
            attempted_at_ms: None,
            stale_at_ms: None,
            windows: Vec::new(),
            error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct GoViewV1 {
    pub version: u16,
    pub profile: String,
    pub state: GoStateV1,
    pub connection_id: Option<String>,
    pub models: u32,
    pub checking: bool,
    pub catalog_checked_at_ms: Option<String>,
    pub last_billing: Option<GoBillingViewV1>,
    pub quota: GoQuotaViewV1,
    pub error: Option<GoFaultV1>,
}
impl GoViewV1 {
    pub fn unavailable(profile: String) -> Self {
        Self {
            version: 1,
            profile,
            state: GoStateV1::Unavailable,
            connection_id: None,
            models: 0,
            checking: false,
            catalog_checked_at_ms: None,
            last_billing: None,
            quota: GoQuotaViewV1 {
                state: GoQuotaStateV1::Unavailable,
                error: Some(GoQuotaFaultV1::Unavailable),
                ..Default::default()
            },
            error: Some(GoFaultV1::Unavailable),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoCommand {
    Status,
    Save,
    Check,
    Quota,
    Disconnect,
}
impl GoCommand {
    pub const ALL: [Self; 5] = [
        Self::Status,
        Self::Save,
        Self::Check,
        Self::Quota,
        Self::Disconnect,
    ];
    pub const fn native(self) -> &'static str {
        match self {
            Self::Status => "go_status",
            Self::Save => "go_save",
            Self::Check => "go_check",
            Self::Quota => "go_quota",
            Self::Disconnect => "go_disconnect",
        }
    }
    pub const fn frontend(self) -> &'static str {
        match self {
            Self::Status => "goStatus",
            Self::Save => "goSave",
            Self::Check => "goCheck",
            Self::Quota => "goQuota",
            Self::Disconnect => "goDisconnect",
        }
    }
    pub fn owns(name: &str) -> bool {
        Self::ALL.iter().any(|command| command.native() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn normalize(s: &str) -> String {
        s.lines().map(str::trim_end).collect::<Vec<_>>().join("\n") + "\n"
    }
    fn generated() -> String {
        let types = specta::Types::default().register::<GoViewV1>();
        let types = specta_typescript::Typescript::default()
            .export(&types, specta_serde::Format)
            .unwrap();
        let mut output = String::from("// Generated by zephium-ipc::opencode_go::tests::export_go_bindings. Do not edit.\nimport { invoke } from \"@tauri-apps/api/core\";\n\n");
        output.push_str(&types);
        output.push_str("\nexport const commands = {\n");
        for command in GoCommand::ALL {
            let (args, values) = if command == GoCommand::Save {
                ("expectedProfile: string, expectedConnectionId: string | null, key: string, billingAcknowledged: boolean", "expectedProfile, expectedConnectionId, key, billingAcknowledged")
            } else if command == GoCommand::Status {
                ("expectedProfile: string", "expectedProfile")
            } else {
                (
                    "expectedProfile: string, expectedConnectionId: string",
                    "expectedProfile, expectedConnectionId",
                )
            };
            output.push_str(&format!(
                "  {}: ({args}): Promise<GoViewV1> => invoke(\"{}\", {{ {values} }}),\n",
                command.frontend(),
                command.native()
            ));
        }
        output.push_str("};\n");
        normalize(&output)
    }
    #[test]
    #[ignore = "Explicit deterministic generation; ordinary tests check drift"]
    fn export_go_bindings() {
        std::fs::write("../../frame/src/shared/ipc/go-bindings.ts", generated()).unwrap();
    }
    #[test]
    fn generated_go_bindings_are_current() {
        assert_eq!(
            std::fs::read_to_string("../../frame/src/shared/ipc/go-bindings.ts").unwrap(),
            generated()
        );
    }
    fn definition(input: &str) -> &str {
        let start = input.find("export type WorkModelProvider = ").unwrap();
        let end = start + input[start..].find(';').unwrap() + 1;
        &input[start..end]
    }
    fn generated_provider() -> String {
        let types =
            specta::Types::default().register::<zephium_core::work::model::WorkModelProvider>();
        let exported = specta_typescript::Typescript::default()
            .export(&types, specta_serde::Format)
            .unwrap();
        normalize(definition(&exported)).trim_end().to_owned()
    }
    /// Regenerates only this type from its authoritative Rust enum for portable
    /// development. The original complete desktop binding exporter stays intact.
    #[test]
    #[ignore = "Explicit scoped generation; full desktop exporter is unchanged"]
    fn export_work_provider_binding() {
        let path = "../../frame/src/shared/ipc/bindings.ts";
        let text = std::fs::read_to_string(path).unwrap();
        let old = definition(&text);
        assert_eq!(text.matches("export type WorkModelProvider = ").count(), 1);
        std::fs::write(path, text.replacen(old, &generated_provider(), 1)).unwrap();
    }
    #[test]
    fn generated_work_provider_type_is_current() {
        let text = std::fs::read_to_string("../../frame/src/shared/ipc/bindings.ts").unwrap();
        assert_eq!(definition(&text), generated_provider());
    }
    #[test]
    fn projection_has_no_secret_plan_or_invented_quota_claim() {
        let value = serde_json::to_value(GoViewV1::unavailable("fixture".into())).unwrap();
        let fields = value.as_object().unwrap();
        assert_eq!(fields.len(), 10);
        for absent in ["key", "access_token", "plan", "remaining", "email"] {
            assert!(!fields.contains_key(absent));
        }
        assert_eq!(value["state"], "unavailable");
        assert!(value["last_billing"].is_null());
    }
    #[test]
    fn command_registration_retains_profile_and_chrome_guards() {
        let native = std::fs::read_to_string("../../desktop/src/opencode_go.rs").unwrap();
        let root = std::fs::read_to_string("../../desktop/src/lib.rs").unwrap();
        assert_eq!(
            native.matches("#[tauri::command]").count(),
            GoCommand::ALL.len()
        );
        for command in GoCommand::ALL {
            assert!(native.contains(&format!("pub(crate) async fn {}(", command.native())));
            let capture = native
                .find("let Some(scope_epoch) = registry.capture_scope(&expected)")
                .unwrap();
            let fetch = native[capture..]
                .find("allowed(&caller, &expected, action).await")
                .unwrap()
                + capture;
            assert!(
                capture < fetch,
                "capture scope before the first actor await"
            );
            assert_eq!(
                native
                    .matches("registry.owns_scope(&expected, scope_epoch)")
                    .count(),
                2
            );
            assert!(
                native.contains("if *foreground != current"),
                "metadata-only projections must not rotate scope"
            );
            assert!(root.contains(&format!("opencode_go::{},", command.native())));
        }
        assert!(native.contains("authorize(caller, CallerPolicy::Main, action.native())"));
        assert!(
            native.contains("profiles.private || profiles.current.as_deref() != Some(expected)")
        );
        assert!(root.contains("opencode_go::profiles_changed(&view)"));
        assert!(root.contains("GoCommand::owns(invoke.message.command())"));
        for command in [
            GoCommand::Save,
            GoCommand::Check,
            GoCommand::Quota,
            GoCommand::Disconnect,
        ] {
            let signature = native
                .split(&format!("pub(crate) async fn {}(", command.native()))
                .nth(1)
                .unwrap()
                .split(") -> GoViewV1")
                .next()
                .unwrap();
            let expected = if command == GoCommand::Save {
                "expected_connection_id: Option<String>"
            } else {
                "expected_connection_id: String"
            };
            assert!(
                signature.contains(expected),
                "mutation must carry its compare-and-swap identity"
            );
        }
        assert!(native.contains(
            ".configure_scoped(service_scope, expected_connection_id, key, acknowledged)"
        ));
        assert!(native.contains(".check_models_scoped(service_scope, expected_connection_id)"));
        assert!(native.contains(".check_quota_scoped(service_scope, expected_connection_id)"));
        assert!(native.contains(".disconnect_scoped(service_scope, expected_connection_id)"));
        assert!(!native.contains("Command::new"));
        assert!(!native.contains("ShellExecute"));
    }
    #[test]
    fn all_subscription_registries_latch_shutdown_before_creation_and_scope_admission() {
        // Portable wiring check; the actual native registry tests run on Windows.
        for name in ["chatgpt", "local_gateway", "opencode_go"] {
            let native = std::fs::read_to_string(format!("../../desktop/src/{name}.rs")).unwrap();
            let create = native.split("fn service(").nth(1).unwrap();
            let lock = create
                .find("let mut services = self.services.lock()")
                .unwrap();
            let closed = create.find("if self.closed.load(").unwrap();
            let existing = create.find("if let Some(service) = services.get(").unwrap();
            assert!(lock < closed && closed < existing);
            assert!(native.contains("super::super::subscription_registry::close("));
            assert!(native.contains("registry.close();"));
            let observe = native
                .split("pub(super) fn profiles_changed(")
                .nth(1)
                .unwrap();
            assert!(
                observe.find("if registry.closed.load(").unwrap()
                    < observe.find("registry.foreground.write()").unwrap()
            );
            for method in ["fn capture_scope(", "fn admit_scope("] {
                let body = native
                    .split(method)
                    .nth(1)
                    .unwrap()
                    .split("\n        fn ")
                    .next()
                    .unwrap();
                assert!(body.contains("self.closed.load("));
            }
        }
    }
}
