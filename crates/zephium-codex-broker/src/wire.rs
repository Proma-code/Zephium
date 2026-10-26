//! Private original app-server wire composition. No arbitrary RPC/config input.
use crate::{catalog::ModelChoice, conversation::Command, text_policy::PreparedText};
use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{json, Map, Value};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Envelope,
    Unsupported,
    Policy,
}
pub(crate) enum Incoming {
    Reply {
        id: u64,
        result: Result<Value, Value>,
    },
    Notification {
        method: String,
        params: Value,
    },
}
// Duplicate keys are refused at every nesting level before a Value can hide one.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unique JSON value")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_none<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut v = Vec::new();
                while let Some(Unique(x)) = a.next_element()? {
                    v.push(x);
                }
                Ok(Unique(Value::Array(v)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut v = Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if v.contains_key(&k) {
                        return Err(de::Error::custom("duplicate key"));
                    }
                    let Unique(x) = a.next_value()?;
                    v.insert(k, x);
                }
                Ok(Unique(Value::Object(v)))
            }
        }
        d.deserialize_any(V)
    }
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Incoming, Fault> {
    if bytes.is_empty() || bytes.len() > crate::protocol::MAX_REPLY_BYTES {
        return Err(Fault::Envelope);
    }
    let Unique(value) = serde_json::from_slice(bytes).map_err(|_| Fault::Envelope)?;
    let o = value.as_object().ok_or(Fault::Envelope)?;
    if o.contains_key("id") {
        if o.contains_key("method") {
            return Err(Fault::Unsupported);
        }
        let id = o
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or(Fault::Envelope)?;
        if o.len() != 2 {
            return Err(Fault::Envelope);
        }
        let result = match (o.get("result"), o.get("error")) {
            (Some(v), None) if v.is_object() => Ok(v.clone()),
            (None, Some(v)) if v.is_object() => Err(v.clone()),
            _ => return Err(Fault::Envelope),
        };
        return Ok(Incoming::Reply { id, result });
    }
    if o.keys()
        .any(|k| !matches!(k.as_str(), "method" | "params" | "emittedAtMs"))
    {
        return Err(Fault::Envelope);
    }
    if o.get("emittedAtMs")
        .is_some_and(|v| v.as_i64().is_none_or(|n| n <= 0))
    {
        return Err(Fault::Envelope);
    }
    let method = o
        .get("method")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
        .ok_or(Fault::Envelope)?;
    let params = o
        .get("params")
        .filter(|v| v.is_object())
        .ok_or(Fault::Envelope)?;
    Ok(Incoming::Notification {
        method: method.into(),
        params: params.clone(),
    })
}

pub(crate) fn model_list(id: u64, cursor: Option<&str>) -> Value {
    json!({"id":id,"method":"model/list","params":{"limit":100,"includeHidden":false,"cursor":cursor}})
}
fn open_params(
    choice: &ModelChoice,
    cwd: &str,
    text: &PreparedText,
    projection: crate::compatibility::ConfigProjection,
) -> Value {
    let mut config = projection.fixed_config(text);
    config["model_reasoning_effort"] = json!(choice.effort().as_str());
    json!({"model":choice.model(),"modelProvider":"openai","cwd":cwd,"approvalPolicy":"never","sandbox":"read-only","serviceTier":null,"config":config})
}
pub(crate) fn command(
    command: &Command,
    cwd: &str,
    prepared: &mut Option<PreparedText>,
    projection: crate::compatibility::ConfigProjection,
) -> Result<Value, Fault> {
    match command {
        Command::StartThread { request_id, choice } => {
            let mut params = open_params(
                choice,
                cwd,
                prepared.as_ref().ok_or(Fault::Policy)?,
                projection,
            );
            params["environments"] = json!([]);
            params["ephemeral"] = json!(false);
            params["allowProviderModelFallback"] = json!(false);
            params["dynamicTools"] = json!([]);
            params["selectedCapabilityRoots"] = json!([]);
            Ok(json!({"id":request_id,"method":"thread/start","params":params}))
        }
        Command::ResumeThread {
            request_id,
            thread_id,
            choice,
        } => {
            let mut params = open_params(
                choice,
                cwd,
                prepared.as_ref().ok_or(Fault::Policy)?,
                projection,
            );
            params["threadId"] = json!(thread_id);
            params["excludeTurns"] = json!(true);
            Ok(json!({"id":request_id,"method":"thread/resume","params":params}))
        }
        Command::StartTurn {
            request_id,
            thread_id,
            choice,
        } => {
            let input = prepared.take().ok_or(Fault::Policy)?.into_turn_input();
            Ok(
                json!({"id":request_id,"method":"turn/start","params":{"threadId":thread_id,"input":input,"model":choice.model(),"effort":choice.effort().as_str(),"environments":[]}}),
            )
        }
        Command::Interrupt {
            request_id,
            thread_id,
            turn_id,
        } => Ok(
            json!({"id":request_id,"method":"turn/interrupt","params":{"threadId":thread_id,"turnId":turn_id}}),
        ),
        Command::Unsubscribe {
            request_id,
            thread_id,
        } => Ok(
            json!({"id":request_id,"method":"thread/unsubscribe","params":{"threadId":thread_id}}),
        ),
    }
}
/// Check actual effective original state before any prompt is consumed.
#[cfg(test)]
pub(crate) fn opened(result: &Value, cwd: &str, choice: &ModelChoice) -> Result<(), Fault> {
    if result.get("cwd").and_then(Value::as_str) != Some(cwd)
        || result.get("model").and_then(Value::as_str) != Some(choice.model())
        || result.get("modelProvider").and_then(Value::as_str) != Some("openai")
        || result.get("reasoningEffort").and_then(Value::as_str) != Some(choice.effort().as_str())
        || result.get("approvalPolicy").and_then(Value::as_str) != Some("never")
        || result
            .pointer("/thread/environments")
            .and_then(Value::as_array)
            .is_none_or(|v| !v.is_empty())
        || result.pointer("/sandbox/type").and_then(Value::as_str) != Some("readOnly")
        || result
            .pointer("/sandbox/networkAccess")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(Fault::Policy);
    }
    Ok(())
}
pub(crate) fn closing_resume(error: &Value, thread: &str) -> bool {
    let Some(o) = error.as_object() else {
        return false;
    };
    o.get("code").and_then(Value::as_i64) == Some(-32600)
        && o.get("message").and_then(Value::as_str)
            == Some(
                format!(
                    "thread {thread} is closing; retry thread/resume after the thread is closed"
                )
                .as_str(),
            )
        && o.get("data").is_none_or(Value::is_null)
        && o.keys()
            .all(|k| matches!(k.as_str(), "code" | "message" | "data"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_keys_requests_and_uncorrelated_shapes_are_refused() {
        for v in [
            br#"{"id":1,"id":2,"result":{}}"#.as_slice(),
            br#"{"id":1,"result":{"model":"a","model":"b"}}"#,
            br#"{"id":1,"method":"tool/call","params":{}}"#,
            br#"{"id":"1","result":{}}"#,
            br#"{"id":1,"result":{},"error":{}}"#,
        ] {
            assert!(decode(v).is_err());
        }
        assert!(matches!(
            decode(br#"{"id":1,"result":{}}"#),
            Ok(Incoming::Reply { id: 1, .. })
        ));
    }
    #[test]
    fn closing_retry_is_exact_and_never_generic() {
        let e = json!({"code":-32600,"message":"thread t is closing; retry thread/resume after the thread is closed"});
        assert!(closing_resume(&e, "t"));
        assert!(!closing_resume(&e, "other"));
        let mut wrong = e;
        wrong["code"] = json!(-32603);
        assert!(!closing_resume(&wrong, "t"));
    }
}
#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::{
        catalog::{CatalogBuilder, Effort},
        text_policy::prepare_text,
    };
    pub(super) fn choice() -> ModelChoice {
        let mut builder = CatalogBuilder::new();
        builder.push_page(None,&json!({"data":[{"id":"picker","model":"original-model","displayName":"Original Model","description":"fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"fixture"}],"defaultReasoningEffort":"high","isDefault":true,"inputModalities":["text"]}],"nextCursor":null})).unwrap();
        builder
            .finish()
            .unwrap()
            .select("picker", Effort::High)
            .unwrap()
    }
    fn result() -> Value {
        json!({"cwd":"C:\\owned\\work","model":"original-model","modelProvider":"openai","reasoningEffort":"high","approvalPolicy":"never","sandbox":{"type":"readOnly","networkAccess":false},"thread":{"environments":[]}})
    }
    #[test]
    fn exact_text_and_cold_override_are_inseparable_until_turn() {
        let mut text = Some(prepare_text("مرحبا $possible_skill".into()).unwrap());
        let choice = choice();
        let start = command(
            &Command::StartThread {
                request_id: 1,
                choice: choice.clone(),
            },
            "C:\\owned\\work",
            &mut text,
            crate::compatibility::ConfigProjection::V164Plus,
        )
        .unwrap();
        assert!(text.is_some());
        assert_eq!(start["params"]["environments"], json!([]));
        assert_eq!(start["params"]["dynamicTools"], json!([]));
        assert_eq!(start["params"]["allowProviderModelFallback"], false);
        assert_eq!(start["params"]["config"]["model_reasoning_effort"], "high");
        assert_eq!(
            start["params"]["config"]["skills.config"],
            json!([{"name":"possible_skill","enabled":false}])
        );
        let resume = command(
            &Command::ResumeThread {
                request_id: 2,
                thread_id: "owned".into(),
                choice: choice.clone(),
            },
            "C:\\owned\\work",
            &mut text,
            crate::compatibility::ConfigProjection::V164Plus,
        )
        .unwrap();
        assert!(resume["params"].get("environments").is_none());
        assert!(resume["params"].get("path").is_none());
        assert!(resume["params"].get("history").is_none());
        assert_eq!(resume["params"]["config"], start["params"]["config"]);
        let turn = Command::StartTurn {
            request_id: 3,
            thread_id: "owned".into(),
            choice,
        };
        let encoded = command(
            &turn,
            "C:\\owned\\work",
            &mut text,
            crate::compatibility::ConfigProjection::V164Plus,
        )
        .unwrap();
        assert!(text.is_none());
        assert!(command(
            &turn,
            "C:\\owned\\work",
            &mut text,
            crate::compatibility::ConfigProjection::V164Plus
        )
        .is_err());
        assert_eq!(
            encoded["params"],
            json!({"threadId":"owned","input":[{"type":"text","text":"مرحبا $possible_skill","text_elements":[]}],"model":"original-model","effort":"high","environments":[]})
        );
    }
    #[test]
    fn effective_response_requires_actual_empty_environment_and_policy() {
        let good = result();
        assert_eq!(opened(&good, "C:\\owned\\work", &choice()), Ok(()));
        for (pointer, value) in [
            ("/thread/environments", Value::Null),
            ("/thread/environments", json!([{"id":"local"}])),
            ("/sandbox/networkAccess", json!(true)),
            ("/sandbox/networkAccess", Value::Null),
            ("/sandbox/type", json!("dangerFullAccess")),
            ("/approvalPolicy", json!("on-request")),
            ("/cwd", json!("C:\\other")),
            ("/model", json!("fallback")),
            ("/reasoningEffort", json!("max")),
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                opened(&bad, "C:\\owned\\work", &choice()),
                Err(Fault::Policy)
            );
        }
        let mut absent = good;
        absent["thread"]
            .as_object_mut()
            .unwrap()
            .remove("environments");
        assert!(opened(&absent, "C:\\owned\\work", &choice()).is_err());
    }
}

/// Closed original advisory vocabulary; discarded values never confer account,
/// execution, closure, completion or quota authority. Whole frame stays bounded.
pub(crate) fn advisory(method: &str, params: &Value) -> Result<bool, Fault> {
    fn id(v: Option<&Value>) -> bool {
        v.and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
    }
    match method {
        // Exact pinned bespoke_event_handling emits this during TokenCount.
        "account/rateLimits/updated" => {
            let object = params.as_object().ok_or(Fault::Envelope)?;
            if object.len() != 1 || !object.get("rateLimits").is_some_and(Value::is_object) {
                return Err(Fault::Envelope);
            }
            Ok(true)
        }
        "thread/started" => {
            if !id(params.pointer("/thread/id")) {
                return Err(Fault::Envelope);
            }
            Ok(true)
        }
        "thread/status/changed" => {
            if !id(params.get("threadId"))
                || !matches!(
                    params.pointer("/status/type").and_then(Value::as_str),
                    Some("notLoaded" | "idle" | "systemError" | "active")
                )
            {
                return Err(Fault::Envelope);
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}
#[cfg(test)]
mod advisory_tests {
    use super::*;
    #[test]
    fn only_closed_telemetry_methods_are_discarded_without_authority() {
        assert_eq!(
            advisory(
                "account/rateLimits/updated",
                &json!({"rateLimits":{"primary":null}})
            ),
            Ok(true)
        );
        assert_eq!(
            advisory("account/rateLimits/updated", &json!({"rateLimits":null})),
            Err(Fault::Envelope)
        );
        assert_eq!(advisory("account/updated", &json!({})), Ok(false));
        assert_eq!(advisory("tool/call", &json!({})), Ok(false));
        assert_eq!(
            advisory(
                "thread/status/changed",
                &json!({"threadId":"t","status":{"type":"active","activeFlags":[]}})
            ),
            Ok(true)
        );
        assert_eq!(
            advisory("thread/closed", &json!({"threadId":"t"})),
            Ok(false)
        );
        let Incoming::Notification{method,params}=decode(br#"{"method":"account/rateLimits/updated","params":{"rateLimits":{}},"emittedAtMs":1}"#).unwrap() else{panic!("notification")};
        assert_eq!(advisory(&method, &params), Ok(true));
        let Incoming::Reply { result, .. } =
            decode(br#"{"id":4,"result":{"data":[],"nextCursor":null}}"#).unwrap()
        else {
            panic!("reply")
        };
        assert!(result.unwrap()["data"].is_array());
        assert_eq!(
            model_list(7, Some("opaque")),
            json!({"id":7,"method":"model/list","params":{"limit":100,"includeHidden":false,"cursor":"opaque"}})
        );
    }
}

pub(crate) enum Startup {
    RemoteDisabled,
    AccountInvalidation(Value),
    Reply(Value),
}
pub(crate) fn startup(bytes: &[u8], expected: Option<u64>) -> Result<Startup, Fault> {
    match decode(bytes)? {
        Incoming::Reply { id, result } if expected == Some(id) => {
            result.map(Startup::Reply).map_err(|_| Fault::Envelope)
        }
        Incoming::Notification { method, params } if method == "account/updated" => {
            if bytes.len() > zephium_provider_accounts::codex::MAX_MESSAGE_BYTES {
                return Err(Fault::Envelope);
            }
            Ok(Startup::AccountInvalidation(params))
        }
        Incoming::Notification { method, .. }
            if expected.is_none() && method == "remoteControl/status/changed" =>
        {
            crate::protocol::accept_startup_status(bytes).map_err(|_| Fault::Policy)?;
            Ok(Startup::RemoteDisabled)
        }
        _ => Err(Fault::Envelope),
    }
}
#[cfg(test)]
mod startup_dispatch_tests {
    use super::*;
    #[test]
    fn retained_account_invalidation_does_not_replace_required_startup_receipts() {
        let account = br#"{"method":"account/updated","params":{"authMode":"chatgpt"}}"#;
        for phase in [None, Some(3), Some(4)] {
            assert!(
                matches!(startup(account,phase),Ok(Startup::AccountInvalidation(v)) if v["authMode"]=="chatgpt")
            );
        }
        let remote=br#"{"method":"remoteControl/status/changed","emittedAtMs":1,"params":{"status":"disabled","serverName":"fixture","installationId":"fixture","environmentId":null}}"#;
        assert!(matches!(startup(remote, None), Ok(Startup::RemoteDisabled)));
        assert!(startup(remote, Some(3)).is_err());
        assert!(
            matches!(startup(br#"{"id":3,"result":{}}"#,Some(3)),Ok(Startup::Reply(v)) if v==json!({}))
        );
        for bad in [
            br#"{"id":4,"result":{}}"#.as_slice(),
            br#"{"method":"account/login/completed","params":{}}"#,
            br#"{"id":3,"method":"account/updated","params":{}}"#,
            br#"{"method":"unknown","params":{}}"#,
        ] {
            assert!(startup(bad, Some(3)).is_err());
        }
    }
}

/// An already observed original terminal survives a later same-pump fault.
/// Failure/transport-loss facts follow it; none can replace it with invented output.
pub(crate) fn preserve_observed<T>(mut observed: Vec<T>, pending: &mut Vec<T>) {
    observed.append(pending);
    *pending = observed;
}
#[cfg(test)]
mod delivery_tests {
    use super::*;
    use crate::conversation::{Conversation, Event, Outcome, Scope};
    #[test]
    fn actual_terminal_survives_later_same_pump_protocol_fault() {
        let scope = Scope::new("owned".into(), [1; 16], 1).unwrap();
        let mut conversation = Conversation::new(scope.clone());
        conversation
            .begin_start(&scope, 1, super::policy_tests::choice())
            .unwrap();
        conversation.reply(&scope,1,&json!({"thread":{"id":"t","ephemeral":false,"status":{"type":"idle"}},"model":"original-model","modelProvider":"openai","reasoningEffort":"high"})).unwrap();
        conversation.begin_turn(&scope, 2).unwrap();
        conversation
            .reply(
                &scope,
                2,
                &json!({"turn":{"id":"u","status":"inProgress","error":null}}),
            )
            .unwrap();
        let observed=conversation.notification(&scope,"turn/completed",&json!({"threadId":"t","turn":{"id":"u","status":"completed","error":null,"itemsView":"full","items":[]}})).unwrap();
        // The next frame is a forbidden server request in the SAME pump.
        assert!(decode(br#"{"id":8,"method":"tool/call","params":{}}"#).is_err());
        let mut pending = conversation.transport_lost(&scope).unwrap();
        preserve_observed(observed, &mut pending);
        assert!(
            matches!(pending.first(),Some(Event::Terminal(result)) if result.outcome==Outcome::Completed)
        );
        assert_eq!(
            pending
                .iter()
                .filter(|e| matches!(e, Event::Terminal(_)))
                .count(),
            1
        );
        assert!(!pending
            .iter()
            .any(|e| matches!(e,Event::Terminal(r) if r.outcome==Outcome::Unknown)));
    }
}

/// Product identity, separate from the preserved credential-free A qualifier.
pub(crate) fn initialize() -> Vec<u8> {
    serde_json::to_vec(&json!({"id":1,"method":"initialize","params":{
        "clientInfo":{"name":"zephium_subscription_trial","title":"Zephium Subscription Trial","version":"0.1.0"},
        "capabilities":{"experimentalApi":true,"explicitGatewayOauth":true,"requestAttestation":false}
    }})).expect("fixed JSON value serialization")
}
#[cfg(test)]
mod initialize_tests {
    use super::*;
    #[test]
    fn product_name_does_not_change_the_fixed_capability_contract() {
        let product: Value = serde_json::from_slice(&initialize()).unwrap();
        let qualifier: Value = serde_json::from_slice(&crate::protocol::initialize()).unwrap();
        assert_eq!(product["id"], qualifier["id"]);
        assert_eq!(product["method"], qualifier["method"]);
        assert_eq!(
            product["params"]["capabilities"],
            qualifier["params"]["capabilities"]
        );
        assert_eq!(
            product["params"]["clientInfo"]["name"],
            "zephium_subscription_trial"
        );
        assert_eq!(
            qualifier["params"]["clientInfo"]["name"],
            "zephium_subscription_trial_qualification"
        );
    }
}
