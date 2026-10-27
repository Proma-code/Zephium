//! Fixed configuration for the pinned stock Codex app-server (rust-v0.164.0).
//!
//! Source: openai/codex@6aaac694803004a63c6f393d153c96d22521a6b9.
//! These checks validate a reported configuration, not an OS sandbox or a
//! completed-turn isolation proof. Stage A starts no thread and grants no login.
//! The native launcher separately owns the home/cwd, image and environment.
//! In particular, disabling skill instructions is not a global host-skill
//! discovery switch. Later turn admission needs its own reviewed boundary.

#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
use serde_json::Value;
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
use sha2::{Digest, Sha256};

/// Standalone app-server arguments; no shell, listener socket or config override.
/// app-server/src/main.rs:42–65 supports these exact spellings.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
pub const APP_SERVER_ARGS: &[&str] = &["--strict-config", "--listen", "stdio://"];

/// Owned-home configuration. Keyring failure must remain failure: no file fallback.
/// Features are verified in features/src/lib.rs at the pinned source revision;
/// skills/config and cloud/orchestrator controls are in config/src/{skills_config,
/// config_toml}.rs. Ordinary runtime state I/O is not model filesystem authority.
pub const CONFIG_TOML: &str = r#"model_provider = "openai"
forced_login_method = "chatgpt"
cli_auth_credentials_store = "keyring"
approval_policy = "never"
sandbox_mode = "read-only"
web_search = "disabled"
notify = []
mcp_servers = {}

[features]
shell_tool = false
view_image = false
shell_snapshot = false
shell_snapshot_v2 = false
apps = false
plugins = false
hooks = false
memories = false
external_agent_memory_import = false
goals = false
image_generation = false
multi_agent = false
multi_agent_v2 = false
multi_env_copy_file = false
request_permissions_tool = false
tool_suggest = false
recommended_plugins = false
skill_mcp_dependency_install = false
skill_search = false

[agents]
enabled = false

[skills]
include_instructions = false

[skills.bundled]
enabled = false

[cloud.skills]
enabled = false

[orchestrator.mcp]
enabled = false
"#;

/// Stock exec-server/src/environment_toml.rs:968–991 tests this exact contract.
/// Do not use ignore-user-config, which bypasses this owned-home file.
pub const ENVIRONMENTS_TOML: &str = "default = \"none\"\ninclude_local = false\n";

/// Required false values in `config/read`'s effective `config` object.
/// Missing values are not interpreted as safe defaults.
pub const CONFIG_REQUIRED_FALSE_PATHS: &[&str] = &[
    "/features/shell_tool",
    "/features/view_image",
    "/features/shell_snapshot",
    "/features/shell_snapshot_v2",
    "/features/apps",
    "/features/plugins",
    "/features/hooks",
    "/features/memories",
    "/features/external_agent_memory_import",
    "/features/goals",
    "/features/image_generation",
    "/features/multi_agent",
    "/features/multi_agent_v2",
    "/features/multi_env_copy_file",
    "/features/request_permissions_tool",
    "/features/tool_suggest",
    "/features/recommended_plugins",
    "/features/skill_mcp_dependency_install",
    "/features/skill_search",
    "/agents/enabled",
    "/skills/include_instructions",
    "/skills/bundled/enabled",
    "/cloud/skills/enabled",
    "/orchestrator/mcp/enabled",
];

/// Required fixed strings in `config/read`'s effective `config` object.
pub const CONFIG_REQUIRED_STRING_PAIRS: &[(&str, &str)] = &[
    ("/model_provider", "openai"),
    ("/forced_login_method", "chatgpt"),
    ("/cli_auth_credentials_store", "keyring"),
    ("/approval_policy", "never"),
    ("/sandbox_mode", "read-only"),
    ("/web_search", "disabled"),
];

/// Closed diagnostics only; never echo config values, paths or server text.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigReadbackError {
    /// The RPC result has no ordinary `config` object.
    Shape,
    /// A required explicit setting is absent, wrong-typed or different.
    Setting,
    /// A notification, MCP, plugin or hook configuration was inherited.
    ImportedResources,
    /// The disposable fixture did not report only its exact owned user layer.
    Layers,
    /// Effective requirements differ from the pinned owned-login-only projection.
    Requirements,
}

/// Validate only the result body of `config/read`, not a whole JSON-RPC envelope.
/// The protocol owner separately validates the reply ID/error and config layers,
/// managed requirements, owned paths and future empty environment selections.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
pub fn validate_config_readback(result: &Value) -> Result<(), ConfigReadbackError> {
    let config = result
        .get("config")
        .filter(|value| value.is_object())
        .ok_or(ConfigReadbackError::Shape)?;
    for path in CONFIG_REQUIRED_FALSE_PATHS {
        if config.pointer(path).and_then(Value::as_bool) != Some(false) {
            return Err(ConfigReadbackError::Setting);
        }
    }
    for (path, expected) in CONFIG_REQUIRED_STRING_PAIRS {
        if config.pointer(path).and_then(Value::as_str) != Some(*expected) {
            return Err(ConfigReadbackError::Setting);
        }
    }
    if !config
        .get("notify")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
        || !config
            .get("mcp_servers")
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
    {
        return Err(ConfigReadbackError::ImportedResources);
    }
    for key in ["plugins", "hooks"] {
        if let Some(value) = config.get(key) {
            if !value.is_null() && !value.as_object().is_some_and(serde_json::Map::is_empty) {
                return Err(ConfigReadbackError::ImportedResources);
            }
        }
    }
    validate_fixture_services(config)?;
    Ok(())
}

// ConfigToml serializes these optional overrides as null when absent. Map-valued
// provider definitions default to empty; no built-in endpoint is injected here.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
fn validate_fixture_services(config: &Value) -> Result<(), ConfigReadbackError> {
    // The exact pinned config/defaults.toml supplies this value; its packaged
    // layer/origin is intentionally omitted by the upstream config/read API.
    if config.get("chatgpt_base_url").is_some_and(|value| {
        !value.is_null() && value.as_str() != Some("https://chatgpt.com/backend-api/")
    }) {
        return Err(ConfigReadbackError::ImportedResources);
    }
    for key in [
        "openai_base_url",
        "experimental_realtime_ws_base_url",
        "experimental_realtime_webrtc_call_base_url",
        "experimental_thread_store_endpoint",
        "experimental_thread_store",
        "model_catalog_json",
        "model_instructions_file",
        "instructions",
        "developer_instructions",
        "forced_chatgpt_workspace_id",
        "mcp_enterprise_managed_auth",
        "mcp_oauth_callback_url",
        "mcp_oauth_callback_port",
        "apps",
        "browser_use",
        "computer_use",
        "desktop",
        "profile",
        "projects",
        "permissions",
        "default_permissions",
        "sandbox_workspace_write",
    ] {
        if config.get(key).is_some_and(|value| !value.is_null()) {
            return Err(ConfigReadbackError::ImportedResources);
        }
    }
    for key in ["model_providers", "profiles", "marketplaces"] {
        if config.get(key).is_some_and(|value| {
            !value.is_null() && !value.as_object().is_some_and(serde_json::Map::is_empty)
        }) {
            return Err(ConfigReadbackError::ImportedResources);
        }
    }
    if config
        .pointer("/skills/config")
        .is_some_and(|value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty))
    {
        return Err(ConfigReadbackError::ImportedResources);
    }
    Ok(())
}

#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
fn fixed_layer_config() -> Value {
    let mut config = serde_json::json!({"notify": [], "mcp_servers": {}});
    for path in CONFIG_REQUIRED_FALSE_PATHS {
        insert_fixed(
            &mut config,
            &path[1..].split('/').collect::<Vec<_>>(),
            Value::Bool(false),
        );
    }
    for (path, value) in CONFIG_REQUIRED_STRING_PAIRS {
        insert_fixed(
            &mut config,
            &path[1..].split('/').collect::<Vec<_>>(),
            Value::String((*value).into()),
        );
    }
    config
}

#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
fn insert_fixed(config: &mut Value, path: &[&str], value: Value) {
    if let Some((key, rest)) = path.split_first() {
        if rest.is_empty() {
            config[*key] = value;
        } else {
            insert_fixed(&mut config[*key], rest, value);
        }
    }
}

// Matches pinned config/src/fingerprint.rs:54–65, not raw file-byte hashing.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
fn layer_version(config: &Value) -> String {
    let mut sorted = config.clone();
    sorted.sort_all_objects();
    let hash = Sha256::digest(sorted.to_string().as_bytes());
    let hex = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

/// Strict disposable-CI layer contract, not support for managed user installs.
/// `expected_user_config` comes from the held native fixture layout, never IPC.
/// Pinned config_manager_service.rs filters packagedDefaults out of both lists.
/// Only one exact User layer and an optional empty System layer are admitted.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
pub fn validate_fixture_layers(
    result: &Value,
    expected_user_config: &str,
) -> Result<(), ConfigReadbackError> {
    let fail = ConfigReadbackError::Layers;
    if expected_user_config.is_empty() {
        return Err(fail);
    }
    let layers = result.get("layers").and_then(Value::as_array).ok_or(fail)?;
    if layers.is_empty() || layers.len() > 2 {
        return Err(fail);
    }
    let fixed = fixed_layer_config();
    let name = serde_json::json!({"type": "user", "file": expected_user_config, "profile": null});
    let version = layer_version(&fixed);
    let metadata = serde_json::json!({"name": name, "version": version});
    let user = &layers[0];
    if user.as_object().map(serde_json::Map::len) != Some(3)
        || user.get("name") != metadata.get("name")
        || user.get("version") != metadata.get("version")
        || user.get("config") != Some(&fixed)
    {
        return Err(fail);
    }
    if let Some(system) = layers.get(1) {
        let system_name = system.get("name").and_then(Value::as_object).ok_or(fail)?;
        let file = system_name
            .get("file")
            .and_then(Value::as_str)
            .ok_or(fail)?;
        if system.as_object().map(serde_json::Map::len) != Some(3)
            || system_name.len() != 2
            || system_name.get("type").and_then(Value::as_str) != Some("system")
            || file.is_empty()
            || file.len() > 32_767
            || file.chars().any(char::is_control)
            || system.get("config") != Some(&serde_json::json!({}))
            || system.get("version").and_then(Value::as_str)
                != Some(layer_version(&serde_json::json!({})).as_str())
        {
            return Err(fail);
        }
    }
    let origins = result
        .get("origins")
        .and_then(Value::as_object)
        .ok_or(fail)?;
    if origins.len() != CONFIG_REQUIRED_FALSE_PATHS.len() + CONFIG_REQUIRED_STRING_PAIRS.len() {
        return Err(fail);
    }
    for path in CONFIG_REQUIRED_FALSE_PATHS
        .iter()
        .copied()
        .chain(CONFIG_REQUIRED_STRING_PAIRS.iter().map(|(path, _)| *path))
    {
        let mut origin = path[1..].replace('/', ".");
        // The pinned fingerprint treats this boolean as a structured feature.
        if origin == "features.multi_agent_v2" {
            origin.push_str(".enabled");
        }
        if origins.get(&origin) != Some(&metadata) {
            return Err(fail);
        }
    }
    Ok(())
}

// Pinned protocol/v2/config.rs:413–459. These are explicit nulls in the
// effective owned-login-only projection, not omitted fields or defaults.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
const OWNED_LOGIN_NULL_REQUIREMENTS: &[&str] = &[
    "modelProvider",
    "modelProviders",
    "cliAuthCredentialsStore",
    "chatgptBaseUrl",
    "additionalDeveloperInstructions",
    "allowedApprovalPolicies",
    "allowedApprovalsReviewers",
    "allowedSandboxModes",
    "allowedWindowsSandboxImplementations",
    "allowedPermissionProfiles",
    "defaultPermissions",
    "allowedWebSearchModes",
    "allowManagedHooksOnly",
    "allowBrowserAndComputerUse",
    "allowAppshots",
    "allowRemoteControl",
    "computerUse",
    "browserUse",
    "inAppBrowser",
    "featureRequirements",
    "hooks",
    "enforceResidency",
    "network",
    "application",
    "autoReview",
    "models",
    "sqliteHome",
    "logDir",
    "modelCatalogJson",
    "checkForUpdateOnStartup",
    "allowLoginShell",
    "feedback",
];

/// Validate the pinned effective requirements for this disposable fixture.
/// config_processor.rs:443–463 emits a non-null object even without managed
/// requirements when the owned forced-login setting restricts methods to ChatGPT.
/// Call only alongside the owned config/layer checks. This response has no
/// origins and is not independent proof of every restriction's provenance.
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
pub fn validate_fixture_requirements(result: &Value) -> Result<(), ConfigReadbackError> {
    let fail = ConfigReadbackError::Requirements;
    let object = result.as_object().ok_or(fail)?;
    if object.len() != 2
        || object
            .get("supportsIndependentSpeedModes")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(fail);
    }
    let requirements = object
        .get("requirements")
        .and_then(Value::as_object)
        .ok_or(fail)?;
    if requirements.len() != 33
        || requirements.get("allowedLoginMethods") != Some(&serde_json::json!(["chatgpt"]))
        || OWNED_LOGIN_NULL_REQUIREMENTS
            .iter()
            .any(|key| requirements.get(*key) != Some(&Value::Null))
    {
        return Err(fail);
    }
    Ok(())
}

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub(crate) fn synthetic_readback(owned_config: &str) -> Value {
    let mut config = fixed_layer_config();
    config
        .get_mut("features")
        .and_then(Value::as_object_mut)
        .expect("fixed features")
        .remove("multi_env_copy_file");
    let metadata = serde_json::json!({"name":{"type":"user","file":owned_config,"profile":null},"version":layer_version(&config)});
    let mut user = metadata.clone();
    user["config"] = config.clone();
    let mut origins = serde_json::Map::new();
    for path in CONFIG_REQUIRED_FALSE_PATHS
        .iter()
        .copied()
        .chain(CONFIG_REQUIRED_STRING_PAIRS.iter().map(|(path, _)| *path))
    {
        if path == "/features/multi_env_copy_file" {
            continue;
        }
        let key = if path == "/features/multi_agent_v2" {
            "features.multi_agent_v2.enabled".to_owned()
        } else {
            path[1..].replace('/', ".")
        };
        origins.insert(key, metadata.clone());
    }
    serde_json::json!({"config":config,"layers":[user],"origins":origins})
}
#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub(crate) fn synthetic_requirements() -> Value {
    let mut requirements = serde_json::Map::new();
    for key in OWNED_LOGIN_NULL_REQUIREMENTS {
        requirements.insert((*key).into(), Value::Null);
    }
    requirements.insert("allowedLoginMethods".into(), serde_json::json!(["chatgpt"]));
    serde_json::json!({"requirements":requirements,"supportsIndependentSpeedModes":true})
}

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub(crate) fn synthetic_features() -> Value {
    let data: Vec<_> = CONFIG_REQUIRED_FALSE_PATHS
        .iter()
        .filter_map(|path| path.strip_prefix("/features/"))
        .map(|name| serde_json::json!({"name":name,"enabled":false}))
        .collect();
    serde_json::json!({"data":data,"nextCursor":null})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixed_result() -> Value {
        let config: toml::Value = toml::from_str(CONFIG_TOML).expect("fixed TOML");
        json!({"config": serde_json::to_value(config).expect("TOML to JSON")})
    }

    const OWNED_CONFIG: &str = r"C:\synthetic\owned-home\config.toml";

    fn layered_result() -> Value {
        let mut result = fixed_result();
        let config = result["config"].clone();
        let metadata = json!({
            "name": {"type":"user", "file":OWNED_CONFIG, "profile":null},
            "version": layer_version(&config)
        });
        let mut user = metadata.clone();
        user["config"] = config;
        result["layers"] = json!([user, {
            "name": {"type":"system", "file":r"C:\ProgramData\Codex\config.toml"},
            "version": layer_version(&json!({})), "config": {}
        }]);
        let mut origins = serde_json::Map::new();
        for path in CONFIG_REQUIRED_FALSE_PATHS
            .iter()
            .copied()
            .chain(CONFIG_REQUIRED_STRING_PAIRS.iter().map(|(path, _)| *path))
        {
            let key = if path == "/features/multi_agent_v2" {
                "features.multi_agent_v2.enabled".to_owned()
            } else {
                path[1..].replace('/', ".")
            };
            origins.insert(key, metadata.clone());
        }
        result["origins"] = Value::Object(origins);
        result
    }

    #[test]
    fn fixed_configuration_is_explicit_and_roundtrips() {
        assert_eq!(validate_config_readback(&fixed_result()), Ok(()));
        assert_eq!(fixed_result()["config"], fixed_layer_config());
        let environment: toml::Value = toml::from_str(ENVIRONMENTS_TOML).unwrap();
        let table = environment.as_table().unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table["default"].as_str(), Some("none"));
        assert_eq!(table["include_local"].as_bool(), Some(false));
        assert_eq!(APP_SERVER_ARGS, ["--strict-config", "--listen", "stdio://"]);
        assert!(!APP_SERVER_ARGS.iter().any(|arg| arg.contains("ignore")));
    }

    #[test]
    fn every_required_setting_rejects_changes_and_wrong_types() {
        for path in CONFIG_REQUIRED_FALSE_PATHS {
            for value in [json!(true), Value::Null, json!("false"), json!(0)] {
                let mut result = fixed_result();
                *result["config"].pointer_mut(path).unwrap() = value;
                assert_eq!(
                    validate_config_readback(&result),
                    Err(ConfigReadbackError::Setting)
                );
            }
        }
        for (path, _) in CONFIG_REQUIRED_STRING_PAIRS {
            for value in [json!("unexpected"), Value::Null, json!(false)] {
                let mut result = fixed_result();
                *result["config"].pointer_mut(path).unwrap() = value;
                assert_eq!(
                    validate_config_readback(&result),
                    Err(ConfigReadbackError::Setting)
                );
            }
        }
    }

    #[test]
    fn absent_settings_and_file_auth_fallback_are_refused() {
        let mut result = fixed_result();
        result["config"].as_object_mut().unwrap().remove("features");
        assert_eq!(
            validate_config_readback(&result),
            Err(ConfigReadbackError::Setting)
        );
        let mut result = fixed_result();
        result["config"]["cli_auth_credentials_store"] = json!("file");
        assert_eq!(
            validate_config_readback(&result),
            Err(ConfigReadbackError::Setting)
        );
        assert_eq!(
            validate_config_readback(&json!({})),
            Err(ConfigReadbackError::Shape)
        );
        assert_eq!(
            validate_config_readback(&json!({"config":[]})),
            Err(ConfigReadbackError::Shape)
        );
    }

    #[test]
    fn inherited_execution_inputs_are_refused_without_echoing_values() {
        for (key, value) in [
            ("notify", json!(["synthetic-command"])),
            (
                "mcp_servers",
                json!({"synthetic": {"command":"synthetic-command"}}),
            ),
            ("hooks", json!({"SessionStart": []})),
            ("plugins", json!({"synthetic": {"enabled":true}})),
        ] {
            let mut result = fixed_result();
            result["config"][key] = value;
            assert_eq!(
                validate_config_readback(&result),
                Err(ConfigReadbackError::ImportedResources)
            );
        }
        let mut result = fixed_result();
        result["config"]["unrelated_benign_metadata"] = json!("synthetic");
        assert_eq!(validate_config_readback(&result), Ok(()));
    }

    #[test]
    fn exact_owned_layer_and_origins_match_pinned_schema() {
        let result = layered_result();
        assert_eq!(validate_fixture_layers(&result, OWNED_CONFIG), Ok(()));
        assert_eq!(
            layer_version(&json!({})),
            "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        let mut user_only = result;
        user_only["layers"].as_array_mut().unwrap().pop();
        assert_eq!(validate_fixture_layers(&user_only, OWNED_CONFIG), Ok(()));
    }

    #[test]
    fn layers_reject_imports_disabled_profile_and_wrong_owned_path() {
        for (pointer, value) in [
            ("/layers/0/name/file", json!(r"C:\foreign\config.toml")),
            ("/layers/0/name/profile", json!("foreign")),
            ("/layers/0/version", json!("sha256:wrong")),
            ("/layers/0/config/model_provider", json!("foreign")),
            ("/layers/1/name/type", json!("packagedDefaults")),
            (
                "/layers/1/config",
                json!({"openai_base_url":"https://example.invalid"}),
            ),
            ("/layers/1/version", json!("sha256:wrong")),
        ] {
            let mut result = layered_result();
            *result.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                validate_fixture_layers(&result, OWNED_CONFIG),
                Err(ConfigReadbackError::Layers)
            );
        }
        let mut result = layered_result();
        result["layers"][0]["disabledReason"] = json!("synthetic");
        assert_eq!(
            validate_fixture_layers(&result, OWNED_CONFIG),
            Err(ConfigReadbackError::Layers)
        );
        let mut result = layered_result();
        let duplicate = result["layers"][0].clone();
        result["layers"][1] = duplicate;
        assert_eq!(
            validate_fixture_layers(&result, OWNED_CONFIG),
            Err(ConfigReadbackError::Layers)
        );
    }

    #[test]
    fn origins_require_exact_user_version_for_every_scalar_setting() {
        for path in CONFIG_REQUIRED_FALSE_PATHS
            .iter()
            .copied()
            .chain(CONFIG_REQUIRED_STRING_PAIRS.iter().map(|(path, _)| *path))
        {
            let mut result = layered_result();
            let key = if path == "/features/multi_agent_v2" {
                "features.multi_agent_v2.enabled".to_owned()
            } else {
                path[1..].replace('/', ".")
            };
            result["origins"][&key]["version"] = json!("sha256:wrong");
            assert_eq!(
                validate_fixture_layers(&result, OWNED_CONFIG),
                Err(ConfigReadbackError::Layers)
            );
        }
        let mut result = layered_result();
        let origin = result["origins"]["features.multi_agent_v2.enabled"].clone();
        result["origins"]
            .as_object_mut()
            .unwrap()
            .remove("features.multi_agent_v2.enabled");
        result["origins"]["features.multi_agent_v2"] = origin;
        assert_eq!(
            validate_fixture_layers(&result, OWNED_CONFIG),
            Err(ConfigReadbackError::Layers)
        );
    }

    #[test]
    fn effective_endpoint_and_import_overrides_are_refused() {
        for (key, value) in [
            ("openai_base_url", json!("https://example.invalid")),
            ("chatgpt_base_url", json!("https://example.invalid")),
            (
                "model_providers",
                json!({"openai":{"base_url":"https://example.invalid"}}),
            ),
            ("mcp_enterprise_managed_auth", json!({})),
            (
                "model_instructions_file",
                json!(r"C:\foreign\instructions.md"),
            ),
            ("developer_instructions", json!("synthetic")),
            ("computer_use", json!({})),
        ] {
            let mut result = fixed_result();
            result["config"][key] = value;
            assert_eq!(
                validate_config_readback(&result),
                Err(ConfigReadbackError::ImportedResources)
            );
        }
        let mut result = fixed_result();
        result["config"]["skills"]["config"] = json!([{"name":"synthetic","enabled":true}]);
        assert_eq!(
            validate_config_readback(&result),
            Err(ConfigReadbackError::ImportedResources)
        );
        let mut result = fixed_result();
        result["config"]["openai_base_url"] = Value::Null;
        result["config"]["model_providers"] = json!({});
        result["config"]["chatgpt_base_url"] = json!("https://chatgpt.com/backend-api/");
        assert_eq!(validate_config_readback(&result), Ok(()));
        for value in [
            "https://chatgpt.com/backend-api",
            "https://chatgpt.com.example.invalid/backend-api/",
            "http://chatgpt.com/backend-api/",
        ] {
            result["config"]["chatgpt_base_url"] = json!(value);
            assert_eq!(
                validate_config_readback(&result),
                Err(ConfigReadbackError::ImportedResources)
            );
        }
    }

    // Independently enumerated from pinned protocol/v2/config.rs:413–459 and
    // config_processor.rs:443–601. Do not derive this fixture from the validator.
    fn owned_login_requirements() -> Value {
        json!({"supportsIndependentSpeedModes":true,"requirements":{
            "modelProvider":null,"modelProviders":null,"allowedLoginMethods":["chatgpt"],
            "cliAuthCredentialsStore":null,"chatgptBaseUrl":null,
            "additionalDeveloperInstructions":null,"allowedApprovalPolicies":null,
            "allowedApprovalsReviewers":null,"allowedSandboxModes":null,
            "allowedWindowsSandboxImplementations":null,"allowedPermissionProfiles":null,
            "defaultPermissions":null,"allowedWebSearchModes":null,"allowManagedHooksOnly":null,
            "allowBrowserAndComputerUse":null,"allowAppshots":null,"allowRemoteControl":null,
            "computerUse":null,"browserUse":null,"inAppBrowser":null,"featureRequirements":null,
            "hooks":null,"enforceResidency":null,"network":null,"application":null,
            "autoReview":null,"models":null,"sqliteHome":null,"logDir":null,
            "modelCatalogJson":null,"checkForUpdateOnStartup":null,"allowLoginShell":null,"feedback":null
        }})
    }

    #[test]
    fn effective_requirements_match_owned_chatgpt_policy() {
        let value = owned_login_requirements();
        assert_eq!(value["requirements"].as_object().unwrap().len(), 33);
        assert_eq!(validate_fixture_requirements(&value), Ok(()));
    }

    #[test]
    fn effective_requirements_reject_every_changed_or_missing_field() {
        let original = owned_login_requirements();
        for key in original["requirements"].as_object().unwrap().keys() {
            let mut value = original.clone();
            value["requirements"].as_object_mut().unwrap().remove(key);
            assert_eq!(
                validate_fixture_requirements(&value),
                Err(ConfigReadbackError::Requirements)
            );
            for replacement in [
                json!(true),
                json!(false),
                json!(0),
                json!(""),
                json!({}),
                json!([]),
            ] {
                let mut value = original.clone();
                value["requirements"][key] = replacement;
                assert_eq!(
                    validate_fixture_requirements(&value),
                    Err(ConfigReadbackError::Requirements)
                );
            }
        }
        for methods in [
            Value::Null,
            json!(["api"]),
            json!(["api", "chatgpt"]),
            json!(["chatgpt", "chatgpt"]),
            json!(["Chatgpt"]),
        ] {
            let mut value = original.clone();
            value["requirements"]["allowedLoginMethods"] = methods;
            assert_eq!(
                validate_fixture_requirements(&value),
                Err(ConfigReadbackError::Requirements)
            );
        }
    }

    #[test]
    fn effective_requirements_reject_wrong_metadata_shape_and_unknown_fields() {
        for replacement in [
            Value::Null,
            json!(false),
            json!(1),
            json!("true"),
            json!([]),
            json!({}),
        ] {
            let mut value = owned_login_requirements();
            value["supportsIndependentSpeedModes"] = replacement;
            assert_eq!(
                validate_fixture_requirements(&value),
                Err(ConfigReadbackError::Requirements)
            );
        }
        for replacement in [
            Value::Null,
            json!({}),
            json!([]),
            json!(false),
            json!("null"),
        ] {
            let mut value = owned_login_requirements();
            value["requirements"] = replacement;
            assert_eq!(
                validate_fixture_requirements(&value),
                Err(ConfigReadbackError::Requirements)
            );
        }
        for key in ["requirements", "supportsIndependentSpeedModes"] {
            let mut value = owned_login_requirements();
            value.as_object_mut().unwrap().remove(key);
            assert_eq!(
                validate_fixture_requirements(&value),
                Err(ConfigReadbackError::Requirements)
            );
        }
        let mut value = owned_login_requirements();
        value["unexpected"] = Value::Null;
        assert_eq!(
            validate_fixture_requirements(&value),
            Err(ConfigReadbackError::Requirements)
        );
        let mut value = owned_login_requirements();
        value["requirements"]["unexpected"] = Value::Null;
        assert_eq!(
            validate_fixture_requirements(&value),
            Err(ConfigReadbackError::Requirements)
        );
        let mut value = owned_login_requirements();
        value["requirements"]
            .as_object_mut()
            .unwrap()
            .remove("modelProvider");
        value["requirements"]["model_provider"] = Value::Null;
        assert_eq!(
            validate_fixture_requirements(&value),
            Err(ConfigReadbackError::Requirements)
        );
    }
}

// Fixed synthetic peer only. These values carry no product policy authority;
// the same real validators below the Connection handshake still check them.
