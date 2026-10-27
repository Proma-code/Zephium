//! Credential-free qualification vocabulary bound to original 0.164.0 schemas.
//! No generic RPC sender, login, thread, executor or model-request constructor.
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) const MAX_REPLY_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Envelope,
    Id,
    Initialize,
    UnexpectedAccount,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    id: u64,
    result: Value,
}

/// Each fixed request has one outstanding numeric ID. Server requests,
/// notifications, duplicate envelope keys/IDs and JSON-RPC errors are not a valid receipt.
pub(crate) fn reply(bytes: &[u8], expected: u64) -> Result<Value, Fault> {
    if bytes.is_empty() || bytes.len() > MAX_REPLY_BYTES {
        return Err(Fault::Envelope);
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| Fault::Envelope)?;
    if reply.id != expected {
        return Err(Fault::Id);
    }
    if !reply.result.is_object() {
        return Err(Fault::Envelope);
    }
    Ok(reply.result)
}
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
pub(crate) fn initialize() -> Vec<u8> {
    encoded(json!({"id":1,"method":"initialize","params":{
        "clientInfo":{"name":"zephium_subscription_trial_qualification","title":"Zephium Subscription Trial Qualification","version":"0.1.0"},
        "capabilities":{"experimentalApi":true,"explicitGatewayOauth":true,"requestAttestation":false}
    }}))
}
pub(crate) fn initialized() -> Vec<u8> {
    encoded(json!({"method":"initialized"}))
}
#[cfg(any(test, all(debug_assertions, feature = "native-fixtures")))]
pub(crate) fn account() -> Vec<u8> {
    encoded(json!({"id":2,"method":"account/read","params":{"refreshToken":false}}))
}
pub(crate) fn config(cwd: &str) -> Vec<u8> {
    encoded(json!({"id":3,"method":"config/read","params":{"includeLayers":true,"cwd":cwd}}))
}
pub(crate) fn requirements() -> Vec<u8> {
    encoded(json!({"id":4,"method":"configRequirements/read"}))
}
fn encoded(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).expect("fixed JSON value serialization")
}

pub(crate) fn accept_initialize(value: &Value, expected_home: &str) -> Result<(), Fault> {
    if value.get("codexHome").and_then(Value::as_str) != Some(expected_home)
        || value.get("platformFamily").and_then(Value::as_str) != Some("windows")
        || value.get("platformOs").and_then(Value::as_str) != Some("windows")
        || !value
            .get("userAgent")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
    {
        return Err(Fault::Initialize);
    }
    Ok(())
}
pub(crate) fn accept_account(value: &Value) -> Result<(), Fault> {
    if value.get("account") != Some(&Value::Null)
        || value.get("requiresOpenaiAuth").and_then(Value::as_bool) != Some(true)
        || value.get("workspaceRouting").is_some_and(|v| !v.is_null())
    {
        return Err(Fault::UnexpectedAccount);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_credential_free_vocabulary_is_emitted() {
        for (bytes, method, id) in [
            (initialize(), "initialize", 1),
            (account(), "account/read", 2),
            (config("C:\\owned\\work"), "config/read", 3),
            (requirements(), "configRequirements/read", 4),
        ] {
            let v: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(v["method"], method);
            assert_eq!(v["id"], id);
        }
        let init: Value = serde_json::from_slice(&initialize()).unwrap();
        assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);
        assert_eq!(init["params"]["capabilities"]["explicitGatewayOauth"], true);
        assert_eq!(
            serde_json::from_slice::<Value>(&account()).unwrap()["params"]["refreshToken"],
            false
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&initialized()).unwrap(),
            json!({"method":"initialized"})
        );
    }
    #[test]
    fn unknown_requests_errors_duplicates_and_bad_ids_are_not_receipts() {
        for bytes in [
            br#"{"id":1,"error":{"code":-1}}"#.as_slice(),
            br#"{"id":1,"result":{},"method":"thread/start"}"#,
            br#"{"id":1,"id":1,"result":{}}"#,
            br#"{"id":1,"result":{},"result":{}}"#,
            br#"{"id":"1","result":{}}"#,
            br#"{"method":"account/updated","params":{}}"#,
            br#"{"id":1,"result":null}"#,
        ] {
            assert!(reply(bytes, 1).is_err());
        }
        assert_eq!(reply(br#"{"id":2,"result":{}}"#, 1), Err(Fault::Id));
        assert!(reply(&vec![b' '; MAX_REPLY_BYTES + 1], 1).is_err());
        assert_eq!(reply(br#"{"id":1,"result":{}}"#, 1), Ok(json!({})));
    }
    #[test]
    fn original_home_platform_and_signed_out_state_are_explicit() {
        let good = json!({"codexHome":"C:\\owned","platformFamily":"windows","platformOs":"windows","userAgent":"synthetic"});
        assert_eq!(accept_initialize(&good, "C:\\owned"), Ok(()));
        assert_eq!(
            accept_initialize(&good, "C:\\other"),
            Err(Fault::Initialize)
        );
        assert_eq!(
            accept_account(
                &json!({"account":null,"requiresOpenaiAuth":true,"workspaceRouting":null})
            ),
            Ok(())
        );
        for bad in [
            json!({"requiresOpenaiAuth":true}),
            json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true}),
            json!({"account":null,"requiresOpenaiAuth":false}),
            json!({"account":null,"requiresOpenaiAuth":true,"workspaceRouting":{}}),
        ] {
            assert_eq!(accept_account(&bad), Err(Fault::UnexpectedAccount));
        }
    }
}

/// Only the pinned server's mandatory post-initialize notification is admitted.
/// Source: app-server/src/lib.rs:1264-1288 and protocol/common.rs:2103-2113.
/// This is a phase transition, not a generic notification skip path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum StartupFault {
    Envelope = 1,
    ConfigWarning = 2,
    OtherNotification = 3,
    ServerRequest = 4,
    ReplyInstead = 5,
    RemoteNotDisabled = 6,
    MetadataShape = 7,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StartupNotification {
    method: String,
    params: RemoteStatus,
    emitted_at_ms: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RemoteStatus {
    status: String,
    server_name: String,
    installation_id: String,
    // Value (not Option) keeps omission distinct from an explicit null.
    environment_id: Value,
}
pub(crate) fn accept_startup_status(bytes: &[u8]) -> Result<(), StartupFault> {
    if bytes.is_empty() || bytes.len() > MAX_REPLY_BYTES {
        return Err(StartupFault::Envelope);
    }
    // This first parse only assigns closed failure categories. Successful
    // admission uses deny_unknown_fields structs, including duplicate-key checks.
    let value: Value = serde_json::from_slice(bytes).map_err(|_| StartupFault::Envelope)?;
    let object = value.as_object().ok_or(StartupFault::Envelope)?;
    if object.contains_key("id") {
        return Err(if object.contains_key("method") {
            StartupFault::ServerRequest
        } else {
            StartupFault::ReplyInstead
        });
    }
    match object.get("method").and_then(Value::as_str) {
        Some("remoteControl/status/changed") => {}
        Some("configWarning") => return Err(StartupFault::ConfigWarning),
        Some(_) => return Err(StartupFault::OtherNotification),
        None => return Err(StartupFault::Envelope),
    }
    let notification: StartupNotification =
        serde_json::from_slice(bytes).map_err(|_| StartupFault::Envelope)?;
    if notification.method != "remoteControl/status/changed" {
        return Err(StartupFault::OtherNotification);
    }
    if notification.params.status != "disabled" || !notification.params.environment_id.is_null() {
        return Err(StartupFault::RemoteNotDisabled);
    }
    if notification.emitted_at_ms <= 0
        || [
            &notification.params.server_name,
            &notification.params.installation_id,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
    {
        return Err(StartupFault::MetadataShape);
    }
    Ok(())
}
#[cfg(test)]
mod startup_tests {
    use super::*;
    fn good() -> Value {
        json!({"method":"remoteControl/status/changed","emittedAtMs":1,"params":{"status":"disabled","serverName":"synthetic-host","installationId":"synthetic-installation","environmentId":null}})
    }
    fn accept(value: &Value) -> Result<(), StartupFault> {
        accept_startup_status(&serde_json::to_vec(value).unwrap())
    }
    #[test]
    fn exact_disabled_initial_status_is_a_separate_phase() {
        let status = serde_json::to_vec(&good()).unwrap();
        assert_eq!(accept_startup_status(&status), Ok(()));
        // Duplicated/late startup status is not an account or config reply.
        assert_eq!(reply(&status, 2), Err(Fault::Envelope));
        assert_eq!(reply(&status, 3), Err(Fault::Envelope));
        assert_eq!(
            accept_startup_status(br#"{"id":2,"result":{}}"#),
            Err(StartupFault::ReplyInstead)
        );
    }
    #[test]
    fn warnings_other_methods_and_server_requests_never_advance() {
        assert_eq!(
            accept(&json!({"method":"configWarning","params":{}})),
            Err(StartupFault::ConfigWarning)
        );
        assert_eq!(
            accept(&json!({"method":"account/updated","params":{}})),
            Err(StartupFault::OtherNotification)
        );
        let mut request = good();
        request["id"] = json!(7);
        assert_eq!(accept(&request), Err(StartupFault::ServerRequest));
        let mut wrong = good();
        wrong["method"] = json!(true);
        assert_eq!(accept(&wrong), Err(StartupFault::Envelope));
    }
    #[test]
    fn active_remote_or_missing_null_environment_is_refused() {
        for status in ["connecting", "connected", "errored", "unknown"] {
            let mut value = good();
            value["params"]["status"] = json!(status);
            assert_eq!(accept(&value), Err(StartupFault::RemoteNotDisabled));
        }
        let mut value = good();
        value["params"]["environmentId"] = json!("synthetic");
        assert_eq!(accept(&value), Err(StartupFault::RemoteNotDisabled));
        value["params"]
            .as_object_mut()
            .unwrap()
            .remove("environmentId");
        assert_eq!(accept(&value), Err(StartupFault::Envelope));
    }
    #[test]
    fn timestamp_identity_bounds_and_unknown_fields_are_checked() {
        for timestamp in [json!(0), json!(-1), json!("1"), Value::Null] {
            let mut value = good();
            value["emittedAtMs"] = timestamp;
            assert!(accept(&value).is_err());
        }
        for identity in [json!(""), json!("x\n"), json!("x".repeat(1025)), json!(0)] {
            let mut value = good();
            value["params"]["installationId"] = identity;
            assert!(accept(&value).is_err());
        }
        let mut value = good();
        value["params"]["extra"] = Value::Null;
        assert_eq!(accept(&value), Err(StartupFault::Envelope));
        let mut value = good();
        value["extra"] = Value::Null;
        assert_eq!(accept(&value), Err(StartupFault::Envelope));
    }
    #[test]
    fn duplicate_envelope_or_status_keys_cannot_hide_activation() {
        for bytes in [br#"{"method":"remoteControl/status/changed","method":"remoteControl/status/changed","emittedAtMs":1,"params":{"status":"disabled","serverName":"s","installationId":"i","environmentId":null}}"#.as_slice(),br#"{"method":"remoteControl/status/changed","emittedAtMs":1,"params":{"status":"connected","status":"disabled","serverName":"s","installationId":"i","environmentId":null}}"#] { assert_eq!(accept_startup_status(bytes),Err(StartupFault::Envelope)); }
    }
}
