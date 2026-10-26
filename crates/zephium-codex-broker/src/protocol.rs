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
pub(crate) fn initialize() -> Vec<u8> {
    encoded(json!({"id":1,"method":"initialize","params":{
        "clientInfo":{"name":"zephium_subscription_trial_qualification","title":"Zephium Subscription Trial Qualification","version":"0.1.0"},
        "capabilities":{"experimentalApi":true,"explicitGatewayOauth":true,"requestAttestation":false}
    }}))
}
pub(crate) fn initialized() -> Vec<u8> {
    encoded(json!({"method":"initialized"}))
}
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
