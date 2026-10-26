//! Semantic compatibility for the original CLI, separate from exact CI receipts.
//!
//! Reviewed against official 0.163.0 (83086188ae0614bd882818ff1aa23f3194962012)
//! and 0.164.0 (6aaac694803004a63c6f393d153c96d22521a6b9) protocol/config sources.
//! No release string equality or whole-object field count is an admission rule.
//! Required policy, origin, auth and no-environment facts remain explicit.
//! Additive presentation metadata does not grant a capability. Unsupported
//! non-null managed restrictions fail closed rather than being overridden.
//!
//! These functions accept already bounded, duplicate-free, correlated RPC result
//! bodies. They do not authorize envelopes, server requests, native paths or
//! cold resume. The native actor still owns image authenticity, profile scope,
//! fixed startup files, unchanged input, actual closed barriers and custody.

use std::borrow::Cow;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::catalog::ModelChoice;
use crate::config::{CONFIG_REQUIRED_FALSE_PATHS, CONFIG_REQUIRED_STRING_PAIRS};
use crate::release_admission::OriginalCliArtifact;
use crate::text_policy::PreparedText;

const MULTI_ENV_COPY_PATH: &str = "/features/multi_env_copy_file";
const MULTI_ENV_COPY_LINE: &str = "multi_env_copy_file = false\n";

/// One version-independent managed disk policy. Release compatibility is
/// observed separately and must not churn profile/home/keyring identity.
pub(crate) fn managed_config() -> Cow<'static, str> {
    Cow::Owned(crate::config::CONFIG_TOML.replace(MULTI_ENV_COPY_LINE, ""))
}

/// Stable semantic registration binding. This is policy identity, not proof of
/// current file bytes or native authority; the profile owner validates those.
pub(crate) fn managed_policy_digest() -> [u8; 32] {
    let config = managed_config();
    let environment = crate::config::ENVIRONMENTS_TOML.as_bytes();
    let mut hash = Sha256::new();
    hash.update(b"zephium.codex.text-policy.v2\0");
    hash.update((config.len() as u64).to_le_bytes());
    hash.update(config.as_bytes());
    hash.update((environment.len() as u64).to_le_bytes());
    hash.update(environment);
    hash.finalize().into()
}

/// Native policy selection, not account/process authority. Only the reviewed
/// older release lacks this feature; all other original releases attempt the
/// current explicit policy and must still pass actual compatibility admission.
/// Policy changes require the profile owner's transactional config migration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigProjection {
    V163,
    V164Plus,
}

impl ConfigProjection {
    pub(crate) fn for_artifact(artifact: &OriginalCliArtifact) -> Self {
        if artifact.version() == "0.163.0" {
            Self::V163
        } else {
            Self::V164Plus
        }
    }

    /// Install only at fresh thread/start or proven cold same-ID resume. The
    /// opaque PreparedText supplies unchanged input and exact prospective denies.
    pub(crate) fn fixed_config(self, text: &PreparedText) -> Value {
        let mut config = text.fixed_config();
        if self == Self::V163 {
            config
                .as_object_mut()
                .expect("native fixed config object")
                .remove("features.multi_env_copy_file");
        }
        config
    }

    fn requires(self, path: &str) -> bool {
        path != MULTI_ENV_COPY_PATH
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Initialize,
    Config,
    Layers,
    Requirements,
    Thread,
    Features,
}

/// The original runtime must report the actual native-owned home and platform.
/// A user-agent presentation string is bounded but never version-authentication.
pub(crate) fn validate_initialize(result: &Value, expected_home: &str) -> Result<(), Fault> {
    if !bounded_path(expected_home)
        || crate::protocol::accept_initialize(result, expected_home).is_err()
    {
        return Err(Fault::Initialize);
    }
    Ok(())
}

/// Startup only, before per-thread SessionFlags are installed. The exact native
/// user config is retained; harmless additions to layer descriptors are allowed.
/// New active layers, profiles, imported resources and overridden origins are not.
pub(crate) fn validate_startup_config(
    result: &Value,
    owned_config: &str,
    projection: ConfigProjection,
) -> Result<(), Fault> {
    validate_effective_config(result, projection)?;
    if !bounded_path(owned_config) {
        return Err(Fault::Layers);
    }
    let layers = result
        .get("layers")
        .and_then(Value::as_array)
        .ok_or(Fault::Layers)?;
    if layers.is_empty() || layers.len() > 2 {
        return Err(Fault::Layers);
    }
    let expected = fixed_layer(projection);
    let version = fingerprint(&expected);
    let user = &layers[0];
    if !owned_metadata(user, owned_config, &version)
        || user.get("config") != Some(&expected)
        || user
            .get("disabledReason")
            .is_some_and(|value| !value.is_null())
    {
        return Err(Fault::Layers);
    }
    if let Some(system) = layers.get(1) {
        if system.pointer("/name/type").and_then(Value::as_str) != Some("system")
            || !system
                .pointer("/name/file")
                .and_then(Value::as_str)
                .is_some_and(bounded_path)
            || system.get("config") != Some(&json!({}))
            || system
                .get("disabledReason")
                .is_some_and(|value| !value.is_null())
            || system.get("version").and_then(Value::as_str)
                != Some(fingerprint(&json!({})).as_str())
        {
            return Err(Fault::Layers);
        }
    }
    let origins = result
        .get("origins")
        .and_then(Value::as_object)
        .ok_or(Fault::Layers)?;
    if origins.len() > 256 {
        return Err(Fault::Layers);
    }
    // Extra origin entries (e.g. newly reported empty maps) may point only to
    // this same exact owned layer. An extra origin cannot introduce an override.
    if origins
        .values()
        .any(|metadata| !owned_metadata(metadata, owned_config, &version))
    {
        return Err(Fault::Layers);
    }
    for path in CONFIG_REQUIRED_FALSE_PATHS
        .iter()
        .copied()
        .chain(CONFIG_REQUIRED_STRING_PAIRS.iter().map(|(path, _)| *path))
        .filter(|path| projection.requires(path))
    {
        let origin = origin_key(path);
        if !origins
            .get(&origin)
            .is_some_and(|metadata| owned_metadata(metadata, owned_config, &version))
        {
            return Err(Fault::Layers);
        }
    }
    Ok(())
}

/// Both reviewed releases derive this singleton from forced_login_method.
/// Optional restriction fields may be omitted or null. An unknown non-null
/// restriction is unsupported, not permission to ignore managed policy.
/// This effective projection does not independently prove restriction origin.
pub(crate) fn validate_requirements(result: &Value) -> Result<(), Fault> {
    let object = result.as_object().ok_or(Fault::Requirements)?;
    if object
        .get("supportsIndependentSpeedModes")
        .is_some_and(|value| !value.is_null() && !value.is_boolean())
    {
        return Err(Fault::Requirements);
    }
    let requirements = object
        .get("requirements")
        .and_then(Value::as_object)
        .ok_or(Fault::Requirements)?;
    if requirements.get("allowedLoginMethods") != Some(&json!(["chatgpt"]))
        || requirements
            .iter()
            .any(|(key, value)| key != "allowedLoginMethods" && !value.is_null())
    {
        return Err(Fault::Requirements);
    }
    Ok(())
}

/// Read-only stock feature inspection. No enablement mutation, thread creation,
/// account request or executor connection is requested by this constructor.
pub(crate) fn feature_request(id: u64) -> Result<Value, Fault> {
    if id == 0 {
        return Err(Fault::Features);
    }
    Ok(
        json!({"id":id,"method":"experimentalFeature/list","params":{"cursor":null,"limit":512,"threadId":null}}),
    )
}

/// Require a complete bounded current-config feature inventory. Both reviewed
/// producers map every FeatureSpec to config.features.enabled(spec.id).
/// The older-release exception is selected only from an authenticated original
/// artifact descriptor; absence in an arbitrary future release is not false.
pub(crate) fn validate_features(result: &Value, projection: ConfigProjection) -> Result<(), Fault> {
    let rows = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or(Fault::Features)?;
    if rows.is_empty() || rows.len() > 512 || result.get("nextCursor") != Some(&Value::Null) {
        return Err(Fault::Features);
    }
    let mut names = std::collections::HashSet::new();
    let mut enabled = std::collections::HashMap::new();
    for row in rows {
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| {
                !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
            })
            .ok_or(Fault::Features)?;
        if !names.insert(name) {
            return Err(Fault::Features);
        }
        enabled.insert(name, row.get("enabled"));
    }
    for name in CONFIG_REQUIRED_FALSE_PATHS
        .iter()
        .filter_map(|path| path.strip_prefix("/features/"))
    {
        match enabled.get(name) {
            Some(Some(Value::Bool(false))) => {}
            None if name == "multi_env_copy_file" && projection == ConfigProjection::V163 => {}
            _ => return Err(Fault::Features),
        }
    }
    Ok(())
}

/// The actual loaded thread must expose an explicitly empty selection; null or
/// missing environments mean unknown/unloaded, not no executor. Read-only alone
/// is not this proof. Cold same-ID resume and unchanged text remain actor duties.
pub(crate) fn validate_opened_thread(
    result: &Value,
    cwd: &str,
    choice: &ModelChoice,
) -> Result<(), Fault> {
    if !bounded_path(cwd)
        || result.get("cwd").and_then(Value::as_str) != Some(cwd)
        || result.get("model").and_then(Value::as_str) != Some(choice.model())
        || result.get("modelProvider").and_then(Value::as_str) != Some("openai")
        || result.get("reasoningEffort").and_then(Value::as_str) != Some(choice.effort().as_str())
        || result.get("approvalPolicy").and_then(Value::as_str) != Some("never")
        || !result
            .pointer("/thread/environments")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || result.pointer("/sandbox/type").and_then(Value::as_str) != Some("readOnly")
        || result
            .pointer("/sandbox/networkAccess")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(Fault::Thread);
    }
    Ok(())
}

fn bounded_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 32_767 && !path.chars().any(char::is_control)
}

// Same required effective service/control semantics as the Stage A validator.
// The sole common-disk omission is proved separately by validate_features; the
// old-release absence exception is not inferred from config/read omission.
fn validate_effective_config(result: &Value, projection: ConfigProjection) -> Result<(), Fault> {
    let config = result
        .get("config")
        .filter(|value| value.is_object())
        .ok_or(Fault::Config)?;
    for path in CONFIG_REQUIRED_FALSE_PATHS
        .iter()
        .filter(|path| projection.requires(path))
    {
        if config.pointer(path).and_then(Value::as_bool) != Some(false) {
            return Err(Fault::Config);
        }
    }
    // The common disk omission must not be smuggled in as an enabled flag.
    if config
        .pointer(MULTI_ENV_COPY_PATH)
        .is_some_and(|value| value.as_bool() != Some(false))
    {
        return Err(Fault::Config);
    }
    for (path, expected) in CONFIG_REQUIRED_STRING_PAIRS {
        if config.pointer(path).and_then(Value::as_str) != Some(*expected) {
            return Err(Fault::Config);
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
        return Err(Fault::Config);
    }
    for key in [
        "plugins",
        "hooks",
        "model_providers",
        "profiles",
        "marketplaces",
    ] {
        if config.get(key).is_some_and(|value| {
            !value.is_null() && !value.as_object().is_some_and(serde_json::Map::is_empty)
        }) {
            return Err(Fault::Config);
        }
    }
    if config.get("chatgpt_base_url").is_some_and(|value| {
        !value.is_null() && value.as_str() != Some("https://chatgpt.com/backend-api/")
    }) {
        return Err(Fault::Config);
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
            return Err(Fault::Config);
        }
    }
    if config
        .pointer("/skills/config")
        .is_some_and(|value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty))
    {
        return Err(Fault::Config);
    }
    Ok(())
}

fn owned_metadata(value: &Value, owned_config: &str, version: &str) -> bool {
    value.pointer("/name/type").and_then(Value::as_str) == Some("user")
        && value.pointer("/name/file").and_then(Value::as_str) == Some(owned_config)
        && value.pointer("/name/profile").is_none_or(Value::is_null)
        && value.get("version").and_then(Value::as_str) == Some(version)
}

fn fixed_layer(projection: ConfigProjection) -> Value {
    let mut fixed = json!({"notify": [], "mcp_servers": {}});
    for path in CONFIG_REQUIRED_FALSE_PATHS
        .iter()
        .filter(|path| projection.requires(path))
    {
        insert(
            &mut fixed,
            &path[1..].split('/').collect::<Vec<_>>(),
            Value::Bool(false),
        );
    }
    for (path, value) in CONFIG_REQUIRED_STRING_PAIRS {
        insert(
            &mut fixed,
            &path[1..].split('/').collect::<Vec<_>>(),
            json!(value),
        );
    }
    fixed
}

fn insert(root: &mut Value, path: &[&str], value: Value) {
    if let Some((key, rest)) = path.split_first() {
        if rest.is_empty() {
            root[*key] = value;
        } else {
            insert(&mut root[*key], rest, value);
        }
    }
}

fn origin_key(path: &str) -> String {
    let mut key = path[1..].replace('/', ".");
    if key == "features.multi_agent_v2" {
        key.push_str(".enabled");
    }
    key
}

// Same semantic fingerprint as both reviewed config/src/fingerprint.rs producers.
fn fingerprint(value: &Value) -> String {
    let mut sorted = value.clone();
    sorted.sort_all_objects();
    let hash = Sha256::digest(sorted.to_string().as_bytes());
    format!(
        "sha256:{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogBuilder, Effort};

    const OWNED: &str = r"C:\synthetic\owned\config.toml";
    const HOME: &str = r"C:\synthetic\owned";
    const CWD: &str = r"C:\synthetic\owned\work";

    fn projection(version: &str) -> ConfigProjection {
        let api = "https://api.github.com/repos/openai/codex/releases";
        let release = "https://github.com/openai/codex/releases";
        let name = "codex-x86_64-pc-windows-msvc.exe";
        let body = json!({"url":format!("{api}/1"),"assets_url":format!("{api}/1/assets"),"html_url":format!("{release}/tag/rust-v{version}"),"id":1,"tag_name":format!("rust-v{version}"),"draft":false,"prerelease":false,"assets":[{"url":format!("{api}/assets/2"),"id":2,"name":name,"state":"uploaded","size":1,"digest":format!("sha256:{}","1".repeat(64)),"browser_download_url":format!("{release}/download/rust-v{version}/{name}")} ]});
        let query = crate::release_admission::ReleaseQuery::from_version_hint(version).unwrap();
        let url = query.url().to_owned();
        let now = std::time::Instant::now();
        // Synthetic native-origin receipt in a pure test, never a real fetch.
        let artifact = crate::release_admission::OfficialReleaseResponse::from_native_fetch(
            query,
            &url,
            200,
            now,
            now,
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap()
        .decode(now)
        .unwrap();
        ConfigProjection::for_artifact(&artifact)
    }

    fn feature_result(include_copy: bool) -> Value {
        let rows = CONFIG_REQUIRED_FALSE_PATHS.iter()
            .filter_map(|path| path.strip_prefix("/features/"))
            .filter(|name| include_copy || *name != "multi_env_copy_file")
            .map(|name| json!({"name":name,"enabled":false,"stage":"underDevelopment","defaultEnabled":false}))
            .collect::<Vec<_>>();
        json!({"data":rows,"nextCursor":null})
    }

    #[test]
    fn common_disk_policy_and_typed_thread_projection_preserve_other_controls_and_text() {
        let old = projection("0.163.0");
        let current = projection("0.164.0");
        assert_eq!(old, ConfigProjection::V163);
        assert_eq!(current, ConfigProjection::V164Plus);
        assert_eq!(projection("1.200.9"), ConfigProjection::V164Plus);
        assert_eq!(
            crate::config::CONFIG_TOML
                .matches(MULTI_ENV_COPY_LINE)
                .count(),
            1
        );
        assert!(!managed_config().contains(MULTI_ENV_COPY_LINE));
        let hex = |bytes: [u8; 32]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(Sha256::digest(managed_config().as_bytes()).into()),
            "0761cb45f99cb29ee775543220d0b7f35c7d6e0f35cc2ee4e99c773df2dbcd38"
        );
        assert_eq!(
            hex(managed_policy_digest()),
            "144d70d08193973c4fc08605a44bd9d2b53774f669bad2af721b64b094c88001"
        );
        let input = "Keep $50000 and `$review` exactly unchanged.";
        let text = crate::text_policy::prepare_text(input.to_owned()).unwrap();
        let mut old_overrides = old.fixed_config(&text);
        assert!(old_overrides.get("features.multi_env_copy_file").is_none());
        old_overrides["features.multi_env_copy_file"] = json!(false);
        assert_eq!(old_overrides, current.fixed_config(&text));
        assert_eq!(text.into_turn_input()[0]["text"], input);
        for mode in [old, current] {
            assert_eq!(
                validate_startup_config(&config_result(), OWNED, mode),
                Ok(())
            );
        }
    }

    #[test]
    fn full_effective_feature_probe_is_required_for_each_verified_image_projection() {
        assert_eq!(
            feature_request(4).unwrap(),
            json!({"id":4,"method":"experimentalFeature/list","params":{"cursor":null,"limit":512,"threadId":null}})
        );
        assert_eq!(feature_request(0), Err(Fault::Features));
        assert_eq!(
            validate_features(&feature_result(false), projection("0.163.0")),
            Ok(())
        );
        assert_eq!(
            validate_features(&feature_result(false), projection("0.164.0")),
            Err(Fault::Features)
        );
        assert_eq!(
            validate_features(&feature_result(false), projection("1.200.9")),
            Err(Fault::Features)
        );
        for mode in [projection("0.163.0"), projection("0.164.0")] {
            assert_eq!(validate_features(&feature_result(true), mode), Ok(()));
        }
        for index in 0..feature_result(true)["data"].as_array().unwrap().len() {
            for enabled in [json!(true), json!("false"), Value::Null] {
                let mut result = feature_result(true);
                result["data"][index]["enabled"] = enabled;
                assert_eq!(
                    validate_features(&result, ConfigProjection::V164Plus),
                    Err(Fault::Features)
                );
            }
        }
        let mut added = feature_result(true);
        added["data"].as_array_mut().unwrap().push(json!({"name":"future_presentation_feature","enabled":{"not":"interpreted"},"description":"additive"}));
        assert_eq!(
            validate_features(&added, ConfigProjection::V164Plus),
            Ok(())
        );
        added["nextCursor"] = json!("next-page");
        assert_eq!(
            validate_features(&added, ConfigProjection::V164Plus),
            Err(Fault::Features)
        );
        let mut duplicate = feature_result(true);
        let row = duplicate["data"][0].clone();
        duplicate["data"].as_array_mut().unwrap().push(row);
        assert_eq!(
            validate_features(&duplicate, ConfigProjection::V164Plus),
            Err(Fault::Features)
        );
        assert_eq!(
            validate_features(
                &json!({"data":vec![json!({"name":"extra"});513],"nextCursor":null}),
                ConfigProjection::V164Plus
            ),
            Err(Fault::Features)
        );
    }

    fn config_result() -> Value {
        let config = fixed_layer(ConfigProjection::V164Plus);
        let metadata = json!({"name":{"type":"user","file":OWNED,"profile":null},"version":fingerprint(&config)});
        let mut user = metadata.clone();
        user["config"] = config.clone();
        let mut origins = serde_json::Map::new();
        for path in CONFIG_REQUIRED_FALSE_PATHS
            .iter()
            .copied()
            .chain(CONFIG_REQUIRED_STRING_PAIRS.iter().map(|(path, _)| *path))
            .filter(|path| *path != MULTI_ENV_COPY_PATH)
        {
            origins.insert(origin_key(path), metadata.clone());
        }
        json!({"config":config,"layers":[user,{"name":{"type":"system","file":r"C:\ProgramData\Codex\config.toml"},"version":fingerprint(&json!({})),"config":{}}],"origins":origins})
    }

    fn choice() -> ModelChoice {
        let mut builder = CatalogBuilder::new();
        builder.push_page(None, &json!({"data":[{"id":"synthetic","model":"synthetic-wire","displayName":"Synthetic","description":"fixture","isDefault":true,"hidden":false,"inputModalities":["text"],"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}],"defaultReasoningEffort":"high"}],"nextCursor":null})).unwrap();
        builder
            .finish()
            .unwrap()
            .select("synthetic", Effort::High)
            .unwrap()
    }

    fn thread_result() -> Value {
        json!({"thread":{"id":"synthetic-thread","environments":[],"status":{"type":"idle"},"ephemeral":false},"cwd":CWD,"model":"synthetic-wire","modelProvider":"openai","reasoningEffort":"high","approvalPolicy":"never","approvalsReviewer":"user","sandbox":{"type":"readOnly","networkAccess":false}})
    }

    #[test]
    fn semantic_projection_does_not_use_user_agent_version_as_authority() {
        // Wire projection fixtures, not captured native runtime results.
        // 0.163/0.164 ConfigRead schemas match, but 0.163 strict startup
        // rejects the later multi_env_copy_file key in historical Stage A.
        // Common production config and effective-feature proof solve that seam;
        // this UA test still is not native 0.163 compatibility evidence.
        for version in ["0.163.0", "0.164.0", "1.200.9"] {
            assert_eq!(
                validate_initialize(
                    &json!({"codexHome":HOME,"platformFamily":"windows","platformOs":"windows","userAgent":format!("codex_cli_rs/{version}")}),
                    HOME
                ),
                Ok(())
            );
            assert_eq!(
                validate_startup_config(&config_result(), OWNED, ConfigProjection::V164Plus),
                Ok(())
            );
            assert_eq!(
                validate_requirements(
                    &json!({"requirements":{"allowedLoginMethods":["chatgpt"],"hooks":null},"supportsIndependentSpeedModes":true})
                ),
                Ok(())
            );
            assert_eq!(
                validate_opened_thread(&thread_result(), CWD, &choice()),
                Ok(())
            );
        }
        let toml: toml::Value = toml::from_str(&managed_config()).unwrap();
        assert_eq!(
            fixed_layer(ConfigProjection::V164Plus),
            serde_json::to_value(toml).unwrap()
        );
    }

    #[test]
    fn harmless_layer_and_origin_metadata_additions_are_compatible() {
        let mut result = config_result();
        result["displayLabel"] = json!("future presentation");
        result["layers"][0]["displayLabel"] = json!("Owned config");
        result["layers"][0]["name"]["displayLabel"] = json!("User");
        for metadata in result["origins"].as_object_mut().unwrap().values_mut() {
            metadata["displayLabel"] = json!("Owned");
        }
        let metadata = result["origins"]["model_provider"].clone();
        result["origins"]["notify"] = metadata;
        assert_eq!(
            validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
            Ok(())
        );
        result["layers"].as_array_mut().unwrap().pop();
        assert_eq!(
            validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
            Ok(())
        );
    }

    #[test]
    fn required_semantics_cannot_be_dropped_changed_or_imported() {
        for path in CONFIG_REQUIRED_FALSE_PATHS
            .iter()
            .filter(|path| **path != MULTI_ENV_COPY_PATH)
        {
            let mut result = config_result();
            *result["config"].pointer_mut(path).unwrap() = json!(true);
            assert_eq!(
                validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
                Err(Fault::Config)
            );
        }
        for (pointer, value) in [
            ("/layers/0/name/file", json!(r"C:\elsewhere\config.toml")),
            ("/layers/0/name/profile", json!("imported")),
            ("/layers/0/name/type", json!("sessionFlags")),
            ("/layers/0/version", json!("sha256:wrong")),
            ("/layers/1/config", json!({"model_provider":"other"})),
            ("/layers/1/name/type", json!("managed")),
            (
                "/origins/model_provider/name/file",
                json!(r"C:\elsewhere\config.toml"),
            ),
            ("/origins/model_provider/version", json!("sha256:wrong")),
        ] {
            let mut result = config_result();
            *result.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
                Err(Fault::Layers),
                "{pointer}"
            );
        }
        let mut result = config_result();
        result["layers"][0]["config"]["model_providers"] =
            json!({"openai":{"base_url":"https://synthetic.invalid"}});
        assert_eq!(
            validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
            Err(Fault::Layers)
        );
        result = config_result();
        result["origins"]
            .as_object_mut()
            .unwrap()
            .remove("features.shell_tool");
        assert_eq!(
            validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
            Err(Fault::Layers)
        );
        result = config_result();
        result["origins"]["future"] =
            json!({"name":{"type":"system","file":"other"},"version":"wrong"});
        assert_eq!(
            validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
            Err(Fault::Layers)
        );
    }

    #[test]
    fn optional_null_requirements_are_compatible_but_active_restrictions_are_not_ignored() {
        for result in [
            json!({"requirements":{"allowedLoginMethods":["chatgpt"]}}),
            json!({"requirements":{"allowedLoginMethods":["chatgpt"],"futureRestriction":null},"futureLabel":"metadata","supportsIndependentSpeedModes":false}),
        ] {
            assert_eq!(validate_requirements(&result), Ok(()));
        }
        for key in [
            "hooks",
            "modelProviders",
            "allowedApprovalPolicies",
            "futureRestriction",
        ] {
            for value in [
                json!(false),
                json!(true),
                json!({}),
                json!([]),
                json!("restriction"),
            ] {
                let mut result = json!({"requirements":{"allowedLoginMethods":["chatgpt"]}});
                result["requirements"][key] = value;
                assert_eq!(validate_requirements(&result), Err(Fault::Requirements));
            }
        }
        for result in [
            json!({}),
            json!({"requirements":null}),
            json!({"requirements":{}}),
            json!({"requirements":{"allowedLoginMethods":["api","chatgpt"]}}),
            json!({"requirements":{"allowedLoginMethods":"chatgpt"}}),
        ] {
            assert_eq!(validate_requirements(&result), Err(Fault::Requirements));
        }
    }

    #[test]
    fn layer_disable_reasons_are_semantic_not_harmless_metadata() {
        for index in [0, 1] {
            let mut result = config_result();
            result["layers"][index]["disabledReason"] = Value::Null;
            assert_eq!(
                validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
                Ok(())
            );
            result["layers"][index]["disabledReason"] = json!("policy-disabled");
            assert_eq!(
                validate_startup_config(&result, OWNED, ConfigProjection::V164Plus),
                Err(Fault::Layers)
            );
        }
    }

    #[test]
    fn current_empty_environment_selection_is_mandatory_not_read_only_assumption() {
        let choice = choice();
        for (pointer, value) in [
            ("/thread/environments", Value::Null),
            ("/thread/environments", json!([{"name":"local"}])),
            ("/cwd", json!(r"C:\other")),
            ("/model", json!("silent-fallback")),
            ("/modelProvider", json!("other")),
            ("/reasoningEffort", json!("medium")),
            ("/approvalPolicy", json!("on-request")),
            ("/sandbox/type", json!("workspaceWrite")),
            ("/sandbox/networkAccess", json!(true)),
        ] {
            let mut result = thread_result();
            *result.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                validate_opened_thread(&result, CWD, &choice),
                Err(Fault::Thread),
                "{pointer}"
            );
        }
        let mut result = thread_result();
        result["thread"]
            .as_object_mut()
            .unwrap()
            .remove("environments");
        assert_eq!(
            validate_opened_thread(&result, CWD, &choice),
            Err(Fault::Thread)
        );
        result = thread_result();
        result["thread"]["futureDisplayLabel"] = json!("unchanged authority");
        result["futureCursor"] = Value::Null;
        assert_eq!(validate_opened_thread(&result, CWD, &choice), Ok(()));
    }

    #[test]
    fn initialize_still_binds_home_platform_and_bounded_presentation() {
        let good = json!({"codexHome":HOME,"platformFamily":"windows","platformOs":"windows","userAgent":"codex_cli_rs/future","extraPresentation":"safe"});
        assert_eq!(validate_initialize(&good, HOME), Ok(()));
        for (field, value) in [
            ("codexHome", json!("other")),
            ("platformFamily", json!("unix")),
            ("platformOs", json!("linux")),
            ("userAgent", json!("bad\ntext")),
            ("userAgent", json!("")),
        ] {
            let mut result = good.clone();
            result[field] = value;
            assert_eq!(validate_initialize(&result, HOME), Err(Fault::Initialize));
        }
        assert_eq!(validate_initialize(&good, ""), Err(Fault::Initialize));
    }
}
