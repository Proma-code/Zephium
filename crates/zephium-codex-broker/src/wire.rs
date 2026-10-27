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
/// Shared finite lexer only; does not admit a method, event, or server request.
pub(crate) fn unique_value(bytes: &[u8]) -> Result<Value, Fault> {
    if bytes.is_empty() || bytes.len() > crate::protocol::MAX_REPLY_BYTES {
        return Err(Fault::Envelope);
    }
    let Unique(value) = serde_json::from_slice(bytes).map_err(|_| Fault::Envelope)?;
    Ok(value)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Incoming, Fault> {
    let value = unique_value(bytes)?;
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
    if let Some(effort) = choice.explicit_effort() {
        config["model_reasoning_effort"] = json!(effort.as_str());
    }
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
            let mut params = json!({"threadId":thread_id,"input":input,"model":choice.model(),"environments":[]});
            if let Some(effort) = choice.explicit_effort() {
                params["effort"] = json!(effort.as_str());
            }
            Ok(json!({"id":request_id,"method":"turn/start","params":params}))
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
        || !crate::effort_policy::readback(choice, result.get("reasoningEffort"))
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
        catalog::{CatalogBuilder, Effort, EffortSelection},
        text_policy::prepare_text,
    };
    pub(super) fn choice() -> ModelChoice {
        let mut builder = CatalogBuilder::new();
        builder.push_page(None,&json!({"data":[{"id":"picker","model":"original-model","displayName":"Original Model","description":"fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"fixture"}],"defaultReasoningEffort":"high","isDefault":true,"inputModalities":["text"]}],"nextCursor":null})).unwrap();
        builder
            .finish()
            .unwrap()
            .select("picker", EffortSelection::Explicit(Effort::High))
            .unwrap()
    }
    fn result() -> Value {
        json!({"cwd":"C:\\owned\\work","model":"original-model","modelProvider":"openai","reasoningEffort":"high","approvalPolicy":"never","sandbox":{"type":"readOnly","networkAccess":false},"thread":{"environments":[]}})
    }
    #[test]
    fn default_omits_both_effort_controls_and_explicit_none_is_literal() {
        for selection in [
            EffortSelection::Default,
            EffortSelection::Explicit(Effort::None),
        ] {
            let choice = crate::effort_policy::tests::choice(selection);
            let mut text = Some(prepare_text("synthetic".into()).unwrap());
            for cmd in [
                Command::StartThread {
                    request_id: 1,
                    choice: choice.clone(),
                },
                Command::ResumeThread {
                    request_id: 2,
                    thread_id: "owned".into(),
                    choice: choice.clone(),
                },
            ] {
                let value = command(
                    &cmd,
                    "C:\\owned\\work",
                    &mut text,
                    crate::compatibility::ConfigProjection::V164Plus,
                )
                .unwrap();
                let effort = value["params"]["config"].get("model_reasoning_effort");
                match selection {
                    EffortSelection::Default => assert!(effort.is_none()),
                    _ => assert_eq!(effort, Some(&json!("none"))),
                }
            }
            let turn = command(
                &Command::StartTurn {
                    request_id: 3,
                    thread_id: "owned".into(),
                    choice: choice.clone(),
                },
                "C:\\owned\\work",
                &mut text,
                crate::compatibility::ConfigProjection::V164Plus,
            )
            .unwrap();
            let effort = turn["params"].get("effort");
            match selection {
                EffortSelection::Default => assert!(effort.is_none()),
                _ => assert_eq!(effort, Some(&json!("none"))),
            }
            let mut effective = result();
            effective["reasoningEffort"] = match selection {
                EffortSelection::Default => Value::Null,
                _ => json!("none"),
            };
            assert!(opened(&effective, "C:\\owned\\work", &choice).is_ok());
            assert!(crate::compatibility::validate_opened_thread(
                &effective,
                "C:\\owned\\work",
                &choice
            )
            .is_ok());
            effective.as_object_mut().unwrap().remove("reasoningEffort");
            assert!(opened(&effective, "C:\\owned\\work", &choice).is_err());
            assert!(crate::compatibility::validate_opened_thread(
                &effective,
                "C:\\owned\\work",
                &choice
            )
            .is_err());
        }
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

/// Finite notification roles derived from original 0.164.0 protocol/common.rs.
/// This is routing, NOT authorization: account/conversation payloads still pass
/// their owned reducers. Server requests are rejected by `decode` before here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NotificationClass<'a> {
    AccountInvalidation,
    AccountLoginCompleted,
    Conversation,
    ConversationUsage(AdvisoryScope<'a>),
    RemoteAuthority,
    Informational(Advisory<'a>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Advisory<'a> {
    pub(crate) kind: AdvisoryKind,
    pub(crate) scope: AdvisoryScope<'a>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdvisoryScope<'a> {
    Global,
    Thread(&'a str),
    Turn {
        thread_id: &'a str,
        turn_id: &'a str,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdvisoryKind {
    RateLimits,
    ThreadStarted,
    ThreadStatus,
    ThreadName,
    ThreadReadState,
    ThreadQueue,
    SkillsChanged,
    Warning,
    GuardianWarning,
    Deprecation,
    ModelVerification,
    ModerationMetadata,
    SafetyBuffering,
    AuthRecoveryStarted,
    AuthRecoveryCompleted,
}

/// Only these proven informational shapes can be discarded. Returned scope must
/// be joined to the actor's owned thread/turn before discard; values themselves
/// never certify authentication, model selection, quota, completion or closure.
/// Unknown methods, action requests and policy changes are not advisories.
/// Known inert fields may gain bounded additions without becoming authority.
pub(crate) fn classify_notification<'a>(
    method: &str,
    params: &'a Value,
) -> Result<NotificationClass<'a>, Fault> {
    use AdvisoryKind as K;
    use NotificationClass as C;
    if !params.is_object() {
        return Err(Fault::Envelope);
    }
    match method {
        "account/updated" => return Ok(C::AccountInvalidation),
        "account/login/completed" => return Ok(C::AccountLoginCompleted),
        "remoteControl/status/changed" => return Ok(C::RemoteAuthority),
        "thread/tokenUsage/updated" => {
            bounded_metadata(params)?;
            let scope = advisory_scope(params)?;
            require_turn(scope)?;
            crate::conversation::validate_usage_snapshot(
                params.get("tokenUsage").ok_or(Fault::Envelope)?,
            )
            .map_err(|_| Fault::Envelope)?;
            return Ok(C::ConversationUsage(scope));
        }
        // A reroute contradicts allowProviderModelFallback=false; a warning
        // about configuration cannot attest that the fixed policy took effect.
        "configWarning" | "model/rerouted" => return Err(Fault::Policy),
        "thread/closed"
        | "turn/started"
        | "turn/completed"
        | "item/agentMessage/delta"
        | "item/started"
        | "item/completed"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/textDelta"
        | "error" => return Ok(C::Conversation),
        _ => (),
    }
    let kind = match method {
        "account/rateLimits/updated" => K::RateLimits,
        "thread/started" => K::ThreadStarted,
        "thread/status/changed" => K::ThreadStatus,
        "thread/name/updated" => K::ThreadName,
        "thread/readState/changed" => K::ThreadReadState,
        "thread/queue/changed" => K::ThreadQueue,
        "skills/changed" => K::SkillsChanged,
        "warning" => K::Warning,
        "guardianWarning" => K::GuardianWarning,
        "deprecationNotice" => K::Deprecation,
        "model/verification" => K::ModelVerification,
        "turn/moderationMetadata" => K::ModerationMetadata,
        "model/safetyBuffering/updated" => K::SafetyBuffering,
        "modelProvider/authRecoveryStarted" => K::AuthRecoveryStarted,
        "modelProvider/authRecoveryCompleted" => K::AuthRecoveryCompleted,
        _ => return Err(Fault::Unsupported),
    };
    bounded_metadata(params)?;
    let mut scope = advisory_scope(params)?;
    let valid = match kind {
        K::RateLimits => rate_limits(params.get("rateLimits")),
        K::ThreadStarted => {
            let id = identifier(params.pointer("/thread/id"))?;
            if scope != AdvisoryScope::Global && scope != AdvisoryScope::Thread(id) {
                return Err(Fault::Envelope);
            }
            scope = AdvisoryScope::Thread(id);
            true
        }
        K::ThreadStatus => {
            require_thread(scope)?;
            match params.pointer("/status/type").and_then(Value::as_str) {
                Some("notLoaded" | "idle" | "systemError") => true,
                Some("active") => strings(params.pointer("/status/activeFlags")),
                _ => false,
            }
        }
        K::ThreadName => {
            require_thread(scope)?;
            optional(params.get("threadName"), text)
        }
        K::ThreadReadState => {
            require_thread(scope)?;
            read_state(params.get("readState"))
        }
        K::ThreadQueue => {
            require_thread(scope)?;
            true
        }
        // Original watcher emits an empty payload: cache invalidation only.
        // It does not cause a skill lookup/import, restart or capability grant.
        K::SkillsChanged => true,
        K::Warning => text(params.get("message")),
        K::GuardianWarning => {
            require_thread(scope)?;
            text(params.get("message"))
        }
        K::Deprecation => text(params.get("summary")) && optional(params.get("details"), text),
        K::ModelVerification => {
            require_turn(scope)?;
            // Verification labels remain uninterpreted metadata. An additive
            // label can never grant local capability or alter account state.
            strings(params.get("verifications"))
        }
        K::ModerationMetadata => {
            require_turn(scope)?;
            // Pinned schema is JsonValue (including null), not an object map.
            params.get("metadata").is_some()
        }
        K::SafetyBuffering => {
            require_turn(scope)?;
            text(params.get("model"))
                && strings(params.get("useCases"))
                && strings(params.get("reasons"))
                && params.get("showBufferingUi").is_some_and(Value::is_boolean)
                && optional(params.get("fasterModel"), text)
        }
        K::AuthRecoveryStarted | K::AuthRecoveryCompleted => {
            require_turn(scope)?;
            text(params.get("provider")) && text(params.get("message"))
        }
    };
    if !valid {
        return Err(Fault::Envelope);
    }
    Ok(C::Informational(Advisory { kind, scope }))
}
fn identifier(value: Option<&Value>) -> Result<&str, Fault> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .ok_or(Fault::Envelope)
}
fn advisory_scope(params: &Value) -> Result<AdvisoryScope<'_>, Fault> {
    let thread = params.get("threadId").filter(|v| !v.is_null());
    let turn = params.get("turnId").filter(|v| !v.is_null());
    match (thread, turn) {
        (None, None) => Ok(AdvisoryScope::Global),
        (Some(thread), None) => Ok(AdvisoryScope::Thread(identifier(Some(thread))?)),
        (Some(thread), Some(turn)) => Ok(AdvisoryScope::Turn {
            thread_id: identifier(Some(thread))?,
            turn_id: identifier(Some(turn))?,
        }),
        (None, Some(_)) => Err(Fault::Envelope),
    }
}
fn require_thread(scope: AdvisoryScope<'_>) -> Result<(), Fault> {
    if matches!(scope, AdvisoryScope::Thread(_) | AdvisoryScope::Turn { .. }) {
        Ok(())
    } else {
        Err(Fault::Envelope)
    }
}
fn require_turn(scope: AdvisoryScope<'_>) -> Result<(), Fault> {
    if matches!(scope, AdvisoryScope::Turn { .. }) {
        Ok(())
    } else {
        Err(Fault::Envelope)
    }
}
fn text(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|s| s.len() <= crate::protocol::MAX_REPLY_BYTES)
}
fn strings(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|values| values.len() <= 4096 && values.iter().all(|v| text(Some(v))))
}
fn optional(value: Option<&Value>, valid: fn(Option<&Value>) -> bool) -> bool {
    value.is_none_or(|v| v.is_null() || valid(Some(v)))
}
fn object(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_object)
}
fn boolean(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_boolean)
}
fn integer(value: Option<&Value>) -> bool {
    value.and_then(Value::as_i64).is_some()
}
fn integer32(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_i64)
        .is_some_and(|v| i32::try_from(v).is_ok())
}
fn read_state(value: Option<&Value>) -> bool {
    value.is_some_and(|v| {
        v.is_object()
            && identifier(v.get("revision")).is_ok()
            && v.get("firstUnread").is_none_or(|position| {
                position.is_null()
                    || (position.is_object()
                        && match position.get("type").and_then(Value::as_str) {
                            Some("threadStart") => true,
                            Some("turn") => identifier(position.get("turnId")).is_ok(),
                            _ => false,
                        })
            })
    })
}
fn rate_window(value: Option<&Value>) -> bool {
    value.is_some_and(|v| {
        v.is_object()
            && integer32(v.get("usedPercent"))
            && optional(v.get("windowDurationMins"), integer)
            && optional(v.get("resetsAt"), integer)
    })
}
fn credits(value: Option<&Value>) -> bool {
    value.is_some_and(|v| {
        v.is_object()
            && boolean(v.get("hasCredits"))
            && boolean(v.get("unlimited"))
            && optional(v.get("balance"), text)
    })
}
fn spend_limit(value: Option<&Value>) -> bool {
    value.is_some_and(|v| {
        v.is_object()
            && text(v.get("limit"))
            && text(v.get("used"))
            && integer32(v.get("remainingPercent"))
            && integer(v.get("resetsAt"))
    })
}
fn rate_limits(value: Option<&Value>) -> bool {
    value.is_some_and(|v| {
        object(Some(v))
            && [
                "limitId",
                "limitName",
                "normalModelSlug",
                "planType",
                "rateLimitReachedType",
            ]
            .iter()
            .all(|k| optional(v.get(*k), text))
            && optional(v.get("primary"), rate_window)
            && optional(v.get("secondary"), rate_window)
            && optional(v.get("credits"), credits)
            && optional(v.get("individualLimit"), spend_limit)
            && optional(v.get("spendControlReached"), boolean)
    })
}
/// Direct callers cannot bypass the frame's allocation/complexity bounds. No
/// metadata is logged or returned; arbitrary moderation/additive JSON is inert.
fn bounded_metadata(value: &Value) -> Result<(), Fault> {
    let mut todo = vec![(value, 0usize)];
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = todo.pop() {
        nodes += 1;
        if nodes > 131_072 || depth > 64 {
            return Err(Fault::Envelope);
        }
        bytes = bytes.checked_add(1).ok_or(Fault::Envelope)?;
        match value {
            Value::String(s) => bytes = bytes.checked_add(s.len()).ok_or(Fault::Envelope)?,
            Value::Array(items) => {
                if items.len() > 131_072 {
                    return Err(Fault::Envelope);
                }
                todo.extend(items.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(items) => {
                if items.len() > 131_072 {
                    return Err(Fault::Envelope);
                }
                for (key, v) in items {
                    bytes = bytes.checked_add(key.len()).ok_or(Fault::Envelope)?;
                    todo.push((v, depth + 1));
                }
            }
            _ => (),
        }
        if bytes > crate::protocol::MAX_REPLY_BYTES {
            return Err(Fault::Envelope);
        }
    }
    Ok(())
}
#[cfg(test)]
#[path = "wire/notification_tests.rs"]
mod notification_tests;

pub(crate) enum Startup {
    RemoteDisabled,
    Informational(AdvisoryKind),
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
        Incoming::Notification { method, params } => {
            match classify_notification(&method, &params)? {
                NotificationClass::Informational(Advisory {
                    kind,
                    scope: AdvisoryScope::Global,
                }) => Ok(Startup::Informational(kind)),
                _ => Err(Fault::Envelope),
            }
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
