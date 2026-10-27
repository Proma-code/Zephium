//! Original item lifecycle is bounded correlation-only bookkeeping. It cannot
//! mint a dispatch ticket, complete a local effect or replace a provider reply.
use super::*;
#[derive(Clone, Eq, PartialEq)]
struct Identity {
    thread: String,
    turn: String,
    tool: PageTool,
    digest: [u8; 32],
}
struct Seen {
    identity: Identity,
    requested: bool,
    started: bool,
    completed: bool,
    completion_status: Option<String>,
}
pub(crate) struct LifecycleLedger {
    entries: BTreeMap<String, Seen>,
    bound: Option<TurnBinding>,
    retired: bool,
}
impl LifecycleLedger {
    pub(crate) fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            bound: None,
            retired: false,
        }
    }
    pub(crate) fn bind(&mut self, binding: TurnBinding) -> Result<(), Fault> {
        if self.bound.is_some() || self.retired {
            return Err(Fault::Retired);
        }
        if self.entries.values().any(|entry| {
            entry.identity.thread != binding.thread || entry.identity.turn != binding.turn
        }) {
            return Err(Fault::Scope);
        }
        self.bound = Some(binding);
        Ok(())
    }
    fn entry(&mut self, call: String, identity: Identity) -> Result<&mut Seen, Fault> {
        if self.bound.as_ref().is_some_and(|binding| {
            identity.thread != binding.thread || identity.turn != binding.turn
        }) {
            return Err(Fault::Scope);
        }
        if let Some(prior) = self.entries.get(&call) {
            if prior.identity != identity {
                return Err(Fault::Scope);
            }
        } else {
            if self.retired {
                return Err(Fault::Retired);
            }
            if self.entries.len()
                >= if self.bound.is_some() {
                    MAX_CALLS
                } else {
                    MAX_QUEUE
                }
            {
                return Err(Fault::Limit);
            }
            self.entries.insert(
                call.clone(),
                Seen {
                    identity,
                    requested: false,
                    started: false,
                    completed: false,
                    completion_status: None,
                },
            );
        }
        self.entries.get_mut(&call).ok_or(Fault::Scope)
    }
    pub(crate) fn request(&mut self, request: &ToolRequest) -> Result<(), Fault> {
        if self.retired {
            return Err(Fault::Retired);
        }
        let entry = self.entry(
            request.call.clone(),
            Identity {
                thread: request.thread.clone(),
                turn: request.turn.clone(),
                tool: request.tool,
                digest: request.body_digest,
            },
        )?;
        if entry.requested {
            return Err(Fault::Duplicate);
        }
        entry.requested = true;
        Ok(())
    }
    pub(crate) fn notification(
        &mut self,
        method: &str,
        params: &Value,
        manifest: &NativeManifest,
    ) -> Result<bool, Fault> {
        if !matches!(method, "item/started" | "item/completed")
            || params.pointer("/item/type").and_then(Value::as_str) != Some("dynamicToolCall")
        {
            return Ok(false);
        }
        bounded_json(params, MAX_FRAME_BYTES)?;
        let p = params.as_object().ok_or(Fault::Envelope)?;
        let completed = method == "item/completed";
        let time = if completed {
            "completedAtMs"
        } else {
            "startedAtMs"
        };
        if p.get(time).and_then(Value::as_i64).is_none() {
            return Err(Fault::Envelope);
        }
        let item = p
            .get("item")
            .and_then(Value::as_object)
            .ok_or(Fault::Envelope)?;
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .ok_or(Fault::Envelope)?;
        if (completed && !matches!(status, "completed" | "failed"))
            || (!completed && status != "inProgress")
        {
            return Err(Fault::Envelope);
        }
        let (call, identity) = parse_identity(
            p.get("threadId")
                .and_then(Value::as_str)
                .ok_or(Fault::Envelope)?,
            p.get("turnId")
                .and_then(Value::as_str)
                .ok_or(Fault::Envelope)?,
            p.get("item").ok_or(Fault::Envelope)?,
            manifest,
        )?;
        let entry = self.entry(call, identity)?;
        let seen = if completed {
            &mut entry.completed
        } else {
            &mut entry.started
        };
        if *seen {
            return Err(Fault::Duplicate);
        }
        *seen = true;
        if completed {
            entry.completion_status = Some(status.to_owned());
        }
        Ok(true)
    }
    /// Correlation-only full-turn snapshot, including in-progress local work.
    /// It cannot change lifecycle completion or discharge an effect ticket.
    pub(crate) fn terminal_item(
        &self,
        thread: &str,
        turn: &str,
        item: &Value,
        manifest: &NativeManifest,
    ) -> Result<(), Fault> {
        let (call, identity) = parse_identity(thread, turn, item, manifest)?;
        let known = self.entries.get(&call).ok_or(Fault::Scope)?;
        if !known.requested || known.identity != identity {
            return Err(Fault::Scope);
        }
        if known.completion_status.as_ref().is_some_and(|status| {
            item.get("status").and_then(Value::as_str) != Some(status.as_str())
        }) {
            return Err(Fault::Envelope);
        }
        Ok(())
    }
    pub(crate) fn tail(
        method: &str,
        params: &Value,
        manifest: &NativeManifest,
        thread: &str,
        turn: &str,
    ) -> Result<bool, Fault> {
        if params.get("threadId").and_then(Value::as_str) != Some(thread)
            || params.get("turnId").and_then(Value::as_str) != Some(turn)
        {
            return Err(Fault::Scope);
        }
        // This ephemeral validator has no execution ledger or dispatch methods.
        // A valid previously settled tail cannot enter the new attempt's ledger.
        Self::new().notification(method, params, manifest)
    }
    pub(crate) fn contains(&self, thread: &str, turn: &str, call: &str) -> bool {
        self.entries
            .get(call)
            .is_some_and(|entry| entry.identity.thread == thread && entry.identity.turn == turn)
    }
    pub(crate) fn retire(&mut self) {
        self.retired = true;
    }
}
fn parse_identity(
    thread: &str,
    turn: &str,
    item: &Value,
    manifest: &NativeManifest,
) -> Result<(String, Identity), Fault> {
    bounded_json(item, MAX_FRAME_BYTES)?;
    let item = item.as_object().ok_or(Fault::Envelope)?;
    if item.get("type").and_then(Value::as_str) != Some("dynamicToolCall")
        || !matches!(
            item.get("status").and_then(Value::as_str),
            Some("inProgress" | "completed" | "failed")
        )
    {
        return Err(Fault::Envelope);
    }
    for key in ["success", "durationMs"] {
        if item.get(key).is_some_and(|value| {
            !value.is_null()
                && if key == "success" {
                    !value.is_boolean()
                } else {
                    value.as_i64().is_none()
                }
        }) {
            return Err(Fault::Envelope);
        }
    }
    if let Some(contents) = item.get("contentItems").filter(|value| !value.is_null()) {
        let items = contents.as_array().ok_or(Fault::Envelope)?;
        if items.len() > 40 {
            return Err(Fault::Limit);
        }
        for content in items {
            if content.get("type").and_then(Value::as_str) != Some("inputText")
                || content.get("text").and_then(Value::as_str).is_none()
            {
                return Err(Fault::Envelope);
            }
        }
        bounded_json(contents, MAX_RESULT_BYTES)?;
    }
    let string = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|value| valid_id(value))
            .map(str::to_owned)
            .ok_or(Fault::Envelope)
    };
    let thread = string(Some(&Value::String(thread.into())))?;
    let turn = string(Some(&Value::String(turn.into())))?;
    let call = string(item.get("id"))?;
    if item.get("namespace").is_some_and(|value| !value.is_null()) {
        return Err(Fault::Unoffered);
    }
    let tool = PageTool::parse(&string(item.get("tool"))?).ok_or(Fault::Unoffered)?;
    if !manifest.contains(tool) {
        return Err(Fault::Unoffered);
    }
    let arguments = item
        .get("arguments")
        .filter(|value| value.is_object())
        .ok_or(Fault::Envelope)?;
    bounded_json(arguments, MAX_ARGUMENT_BYTES)?;
    let body = json!({"threadId":thread,"turnId":turn,"callId":call,"namespace":null,"tool":tool.name(),"arguments":arguments});
    Ok((
        call,
        Identity {
            thread,
            turn,
            tool,
            digest: digest(&body)?,
        },
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> NativeManifest {
        NativeManifest::page_v1(
            PageTool::ALL
                .into_iter()
                .map(|tool| Definition {
                    name: tool.name().into(),
                    description: "owned".into(),
                    input_schema: json!({"type":"object"}),
                })
                .collect(),
        )
        .unwrap()
    }
    fn item(completed: bool) -> Value {
        let mut value = json!({"threadId":"thread","turnId":"turn","item":{"type":"dynamicToolCall","id":"call","tool":"read_page","arguments":{},"namespace":null,"status":if completed{"completed"}else{"inProgress"},"contentItems":null,"success":null,"durationMs":null}});
        value[if completed {
            "completedAtMs"
        } else {
            "startedAtMs"
        }] = json!(10);
        value
    }
    fn request(manifest: &NativeManifest) -> ToolRequest {
        ToolRequest::decode(&serde_json::to_vec(&json!({"id":"rpc","method":"item/tool/call","params":{"threadId":"thread","turnId":"turn","callId":"call","tool":"read_page","arguments":{}}})).unwrap(),manifest).unwrap()
    }
    fn binding(manifest: &NativeManifest) -> TurnBinding {
        TurnBinding::new(
            Scope::new("profile".into(), [1; 16], 1).unwrap(),
            1,
            "conversation".into(),
            1,
            "thread".into(),
            "turn".into(),
            manifest,
        )
        .unwrap()
    }
    #[test]
    fn lifecycle_before_ack_is_bookkeeping_only_and_joins_exact_body() {
        let manifest = manifest();
        let mut ledger = LifecycleLedger::new();
        assert_eq!(
            ledger.notification("item/started", &item(false), &manifest),
            Ok(true)
        );
        ledger.request(&request(&manifest)).unwrap();
        ledger.bind(binding(&manifest)).unwrap();
        ledger
            .notification("item/completed", &item(true), &manifest)
            .unwrap();
        assert!(ledger
            .notification("item/completed", &item(true), &manifest)
            .is_err());
    }
    #[test]
    fn lifecycle_foreign_turn_or_changed_arguments_cannot_join_request() {
        let manifest = manifest();
        let mut ledger = LifecycleLedger::new();
        let mut changed = item(false);
        changed["item"]["arguments"] = json!({"foreign":true});
        ledger
            .notification("item/started", &changed, &manifest)
            .unwrap();
        assert!(ledger.request(&request(&manifest)).is_err());
        let mut ledger = LifecycleLedger::new();
        changed = item(false);
        changed["turnId"] = json!("foreign");
        ledger
            .notification("item/started", &changed, &manifest)
            .unwrap();
        assert!(ledger.bind(binding(&manifest)).is_err());
    }
    #[test]
    fn lifecycle_retirement_allows_only_known_tail_and_never_new_calls() {
        let manifest = manifest();
        let mut ledger = LifecycleLedger::new();
        ledger.request(&request(&manifest)).unwrap();
        ledger.bind(binding(&manifest)).unwrap();
        ledger.retire();
        assert_eq!(
            ledger.notification("item/completed", &item(true), &manifest),
            Ok(true)
        );
        let mut other = item(false);
        other["item"]["id"] = json!("new");
        assert!(ledger
            .notification("item/started", &other, &manifest)
            .is_err());
        assert_eq!(
            LifecycleLedger::tail("item/completed", &item(true), &manifest, "thread", "turn"),
            Ok(true)
        );
        assert!(
            LifecycleLedger::tail("item/completed", &item(true), &manifest, "thread", "other")
                .is_err()
        );
    }
    #[test]
    fn lifecycle_shape_cannot_restore_host_executor_or_nontext_output() {
        let manifest = manifest();
        for changed in 0..4 {
            let mut value = item(false);
            match changed {
                0 => value["item"]["namespace"] = json!("browser"),
                1 => value["item"]["tool"] = json!("shell"),
                2 => value["startedAtMs"] = json!("10"),
                _ => value["item"]["contentItems"] = json!([{"type":"inputImage","imageUrl":"x"}]),
            };
            assert!(LifecycleLedger::new()
                .notification("item/started", &value, &manifest)
                .is_err());
        }
    }
}
