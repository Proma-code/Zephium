//! Closed, pure original dynamic-tool protocol. No executor, process dispatch,
//! browser authority, authentication, deadline renewal or renderer tool map.
//! Source: original 0.163/0.164 rpc.rs, v2/item.rs and protocol/dynamic_tools.rs.
use crate::conversation::Scope;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

mod manifest;
pub(crate) use manifest::{Definition, NativeManifest, PageTool};
mod lifecycle;
pub(crate) use lifecycle::LifecycleLedger;
mod ledger;
pub(crate) use ledger::{EarlyRequests, EffectOutcome, EffectTicket, ResponseTicket, TurnLedger};

pub(crate) const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RESULT_BYTES: usize = 128 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_QUEUE: usize = 8;
const MAX_CALLS: usize = 40;
const MAX_QUEUE_BYTES: usize = MAX_QUEUE * (MAX_ARGUMENT_BYTES + 2048);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Envelope,
    Limit,
    Manifest,
    Unoffered,
    Scope,
    Duplicate,
    Retired,
    Busy,
    Ticket,
}
/// Server-request identity is independent of client outbound IDs and call IDs.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum RpcId {
    Integer(i64),
    String(String),
}
impl RpcId {
    fn parse(value: &Value) -> Result<Self, Fault> {
        if let Some(number) = value.as_i64() {
            return Ok(Self::Integer(number));
        }
        let text = value
            .as_str()
            .filter(|v| v.len() <= 256)
            .ok_or(Fault::Envelope)?;
        Ok(Self::String(text.into()))
    }
    fn value(&self) -> Value {
        match self {
            Self::Integer(v) => json!(v),
            Self::String(v) => json!(v),
        }
    }
}
/// Validated request only. It is not an execution permit and is not Clone.
pub(crate) struct ToolRequest {
    id: RpcId,
    thread: String,
    turn: String,
    call: String,
    tool: PageTool,
    arguments: Value,
    body_digest: [u8; 32],
    manifest: [u8; 32],
    bytes: usize,
}
impl ToolRequest {
    pub(crate) fn identity_key(&self) -> (&str, &str, &str) {
        (&self.thread, &self.turn, &self.call)
    }

    pub(crate) fn decode(bytes: &[u8], manifest: &NativeManifest) -> Result<Self, Fault> {
        if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
            return Err(Fault::Limit);
        }
        let value = crate::wire::unique_value(bytes).map_err(|_| Fault::Envelope)?;
        let root = value.as_object().ok_or(Fault::Envelope)?;
        if root.len() != 3 || root.get("method").and_then(Value::as_str) != Some("item/tool/call") {
            return Err(Fault::Envelope);
        }
        let id = RpcId::parse(root.get("id").ok_or(Fault::Envelope)?)?;
        let params = root
            .get("params")
            .and_then(Value::as_object)
            .ok_or(Fault::Envelope)?;
        if params.keys().any(|k| {
            !matches!(
                k.as_str(),
                "threadId" | "turnId" | "callId" | "namespace" | "tool" | "arguments"
            )
        }) {
            return Err(Fault::Envelope);
        }
        // The fixed Page manifest is flat. Missing/null means the same original
        // Option::None; even reserved upstream namespaces are never admitted.
        if params.get("namespace").is_some_and(|v| !v.is_null()) {
            return Err(Fault::Unoffered);
        }
        let string = |key| {
            params
                .get(key)
                .and_then(Value::as_str)
                .filter(|v| valid_id(v))
                .map(str::to_owned)
                .ok_or(Fault::Envelope)
        };
        let thread = string("threadId")?;
        let turn = string("turnId")?;
        let call = string("callId")?;
        let tool = PageTool::parse(&string("tool")?).ok_or(Fault::Unoffered)?;
        if !manifest.contains(tool) {
            return Err(Fault::Unoffered);
        }
        let arguments = params
            .get("arguments")
            .filter(|v| v.is_object())
            .ok_or(Fault::Envelope)?
            .clone();
        bounded_json(&arguments, MAX_ARGUMENT_BYTES)?;
        let body = json!({"threadId":thread,"turnId":turn,"callId":call,"namespace":null,"tool":tool.name(),"arguments":arguments});
        let body_digest = digest(&body)?;
        Ok(Self {
            id,
            thread,
            turn,
            call,
            tool,
            arguments,
            body_digest,
            manifest: manifest.digest(),
            bytes: bytes.len(),
        })
    }
}
/// Minted by the native actor only after the original turn ID is known. Page
/// owns its separate live browser/profile/epoch fences and actual executor.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TurnBinding {
    scope: Scope,
    account_epoch: u64,
    conversation: String,
    reply: u64,
    thread: String,
    turn: String,
    manifest: [u8; 32],
}
impl TurnBinding {
    pub(crate) fn new(
        scope: Scope,
        account_epoch: u64,
        conversation: String,
        reply: u64,
        thread: String,
        turn: String,
        manifest: &NativeManifest,
    ) -> Result<Self, Fault> {
        if account_epoch == 0
            || reply == 0
            || ![&conversation, &thread, &turn]
                .into_iter()
                .all(|v| valid_id(v))
        {
            return Err(Fault::Scope);
        }
        Ok(Self {
            scope,
            account_epoch,
            conversation,
            reply,
            thread,
            turn,
            manifest: manifest.digest(),
        })
    }
    fn matches(&self, request: &ToolRequest) -> bool {
        self.thread == request.thread
            && self.turn == request.turn
            && self.manifest == request.manifest
    }
}
fn valid_id(v: &str) -> bool {
    !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control)
}
fn bounded_json(value: &Value, maximum: usize) -> Result<Vec<u8>, Fault> {
    fn walk(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), Fault> {
        *nodes = nodes.checked_add(1).ok_or(Fault::Limit)?;
        if depth > 32 || *nodes > 8192 {
            return Err(Fault::Limit);
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    walk(value, depth + 1, nodes)?;
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    walk(value, depth + 1, nodes)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    walk(value, 0, &mut 0)?;
    fn canonical(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for value in map.values_mut() {
                    canonical(value);
                }
                map.sort_keys();
            }
            Value::Array(values) => {
                for value in values {
                    canonical(value);
                }
            }
            _ => (),
        }
    }
    let mut ordered = value.clone();
    canonical(&mut ordered);
    let bytes = serde_json::to_vec(&ordered).map_err(|_| Fault::Envelope)?;
    if bytes.len() > maximum {
        Err(Fault::Limit)
    } else {
        Ok(bytes)
    }
}
fn digest(value: &Value) -> Result<[u8; 32], Fault> {
    // A recursively key-sorted tagged value, not original whitespace/order,
    // binds admitted semantics even if another dependency enables preserve_order.
    let bytes = bounded_json(value, MAX_FRAME_BYTES)?;
    Ok(Sha256::digest(bytes).into())
}
#[cfg(test)]
mod tests;
