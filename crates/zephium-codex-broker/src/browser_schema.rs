//! Narrow experimental BrowserV1 schema projection, not a JSON Schema engine.
//! Source: official 0.163/0.164 precomputed experimental exports. This pure
//! module has no image, process, account, path, or execution authority. Native
//! custody must join the result to the same admitted image's completed export.
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Fixed exporter filenames, in the same order as `BrowserSchemas::files`.
pub(crate) const FILE_NAMES: [&str; 5] = [
    "ServerRequest.json",
    "DynamicToolCallResponse.json",
    "JSONRPCResponse.json",
    "ClientRequest.json",
    "ServerNotification.json",
];
/// Local parsing ceiling, not an original-runtime limit.
pub(crate) const MAX_FILE_BYTES: usize = 512 * 1024;
const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 64;
const DRAFT: &str = "http://json-schema.org/draft-07/schema#";

/// Native reads exactly these files after validating owned output custody.
/// Raw bytes retain duplicate-key evidence; parsed values cannot replace them.
pub(crate) struct BrowserSchemas<'a> {
    pub(crate) server_requests: &'a [u8],
    pub(crate) tool_response: &'a [u8],
    pub(crate) rpc_response: &'a [u8],
    pub(crate) client_requests: &'a [u8],
    pub(crate) server_notifications: &'a [u8],
}
impl<'a> BrowserSchemas<'a> {
    fn files(self) -> [&'a [u8]; 5] {
        [
            self.server_requests,
            self.tool_response,
            self.rpc_response,
            self.client_requests,
            self.server_notifications,
        ]
    }
}

/// Bounded diagnostic without raw schema data or filesystem paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchemaFault {
    Limit,
    Json,
    Reference,
    Shape,
}

/// Checked structural contract only. Not a native BrowserCapability.
/// No public constructor, default, deserialization or process authority.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BrowserShape {
    digest: [u8; 32],
}
impl BrowserShape {
    /// Stable identity of the checked contract, not a hash of files or image.
    pub(crate) fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// Project only the fixed Browser contract; unknown validation semantics fail
/// closed. Unrelated definitions, routes and documentation confer no authority.
pub(crate) fn project(input: BrowserSchemas<'_>) -> Result<BrowserShape, SchemaFault> {
    let files = input.files();
    if files
        .iter()
        .any(|b| b.is_empty() || b.len() > MAX_FILE_BYTES)
        || files.iter().map(|b| b.len()).sum::<usize>() > MAX_TOTAL_BYTES
    {
        return Err(SchemaFault::Limit);
    }
    let docs = files
        .map(parse)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let [server, response, rpc, client, notifications] = docs.as_slice() else {
        return Err(SchemaFault::Shape);
    };

    route(
        server,
        "item/tool/call",
        "DynamicToolCallParams",
        Direction::Incoming,
    )?;
    request_id(definition(server, "RequestId")?)?;
    let params = object(
        definition(server, "DynamicToolCallParams")?,
        &["arguments", "callId", "threadId", "tool", "turnId"],
        &[],
        &[
            "arguments",
            "callId",
            "namespace",
            "threadId",
            "tool",
            "turnId",
        ],
        false,
    )?;
    for key in ["callId", "threadId", "tool", "turnId"] {
        leaf(field(params, key)?, &json!({"type":"string"}))?;
    }
    leaf(field(params, "arguments")?, &Value::Bool(true))?;
    leaf(
        field(params, "namespace")?,
        &json!({"type":["string","null"]}),
    )?;

    let output = object(
        response,
        &["contentItems", "success"],
        &[],
        &["contentItems", "success"],
        true,
    )?;
    array_ref(
        field(output, "contentItems")?,
        false,
        "DynamicToolCallOutputContentItem",
    )?;
    leaf(field(output, "success")?, &json!({"type":"boolean"}))?;
    input_text(definition(response, "DynamicToolCallOutputContentItem")?)?;
    let envelope = object(rpc, &["id", "result"], &[], &["id", "result"], true)?;
    reference(field(envelope, "id")?, "RequestId")?;
    leaf(field(envelope, "result")?, &Value::Bool(true))?;
    request_id(definition(rpc, "RequestId")?)?;

    route(
        client,
        "thread/start",
        "ThreadStartParams",
        Direction::Outgoing,
    )?;
    route(
        client,
        "thread/resume",
        "ThreadResumeParams",
        Direction::Outgoing,
    )?;
    request_id(definition(client, "RequestId")?)?;
    let start = object(
        definition(client, "ThreadStartParams")?,
        &[],
        &["dynamicTools", "developerInstructions"],
        &["dynamicTools", "developerInstructions"],
        true,
    )?;
    array_ref(field(start, "dynamicTools")?, true, "DynamicToolSpec")?;
    instructions(field(start, "developerInstructions")?)?;
    let function = variant(definition(client, "DynamicToolSpec")?, "type", "function")?;
    let function = object(
        function,
        &["type", "name", "description", "inputSchema"],
        &["deferLoading"],
        &["type", "name", "description", "inputSchema", "deferLoading"],
        true,
    )?;
    tag(field(function, "type")?, "function")?;
    for key in ["name", "description"] {
        leaf(field(function, key)?, &json!({"type":"string"}))?;
    }
    leaf(field(function, "inputSchema")?, &Value::Bool(true))?;
    leaf(field(function, "deferLoading")?, &json!({"type":"boolean"}))?;
    let resume = object(
        definition(client, "ThreadResumeParams")?,
        &["threadId"],
        &["developerInstructions"],
        &["threadId", "developerInstructions"],
        true,
    )?;
    // An optional future declaration grants nothing: the fixed serializer does
    // not send it. Making it required is refused by `object` above.
    leaf(field(resume, "threadId")?, &json!({"type":"string"}))?;
    instructions(field(resume, "developerInstructions")?)?;

    for (method, name, time) in [
        ("item/started", "ItemStartedNotification", "startedAtMs"),
        (
            "item/completed",
            "ItemCompletedNotification",
            "completedAtMs",
        ),
    ] {
        route(notifications, method, name, Direction::Notification)?;
        let item = object(
            definition(notifications, name)?,
            &["threadId", "turnId", "item", time],
            &[],
            &["threadId", "turnId", "item", time],
            false,
        )?;
        for key in ["threadId", "turnId"] {
            leaf(field(item, key)?, &json!({"type":"string"}))?;
        }
        reference(field(item, "item")?, "ThreadItem")?;
        leaf(
            field(item, time)?,
            &json!({"type":"integer","format":"int64"}),
        )?;
    }
    let item = variant(
        definition(notifications, "ThreadItem")?,
        "type",
        "dynamicToolCall",
    )?;
    let item = object(
        item,
        &["arguments", "id", "status", "tool", "type"],
        &[],
        &[
            "arguments",
            "id",
            "status",
            "tool",
            "type",
            "namespace",
            "contentItems",
            "success",
            "durationMs",
        ],
        false,
    )?;
    tag(field(item, "type")?, "dynamicToolCall")?;
    for key in ["id", "tool"] {
        leaf(field(item, key)?, &json!({"type":"string"}))?;
    }
    leaf(field(item, "arguments")?, &Value::Bool(true))?;
    leaf(
        field(item, "namespace")?,
        &json!({"type":["string","null"]}),
    )?;
    leaf(field(item, "success")?, &json!({"type":["boolean","null"]}))?;
    leaf(
        field(item, "durationMs")?,
        &json!({"type":["integer","null"],"format":"int64"}),
    )?;
    array_ref(
        field(item, "contentItems")?,
        true,
        "DynamicToolCallOutputContentItem",
    )?;
    input_text(definition(
        notifications,
        "DynamicToolCallOutputContentItem",
    )?)?;
    reference(field(item, "status")?, "DynamicToolCallStatus")?;
    leaf(
        definition(notifications, "DynamicToolCallStatus")?,
        &json!({"type":"string","enum":["inProgress","completed","failed"]}),
    )?;

    Ok(BrowserShape {
        digest: Sha256::digest(b"zephium.codex.browser-schema.v1\0five-files;string-i64;flat-call;input-text-result;function-spec;developer-resume;no-resume-tools;dynamic-lifecycle").into(),
    })
}

fn parse(bytes: &[u8]) -> Result<Value, SchemaFault> {
    let value = crate::wire::unique_value(bytes).map_err(|_| SchemaFault::Json)?;
    let mut remaining = MAX_NODES;
    bounded(&value, 0, &mut remaining)?;
    if value.get("$schema").and_then(Value::as_str) != Some(DRAFT) {
        return Err(SchemaFault::Shape);
    }
    Ok(value)
}

fn bounded(value: &Value, depth: usize, remaining: &mut usize) -> Result<(), SchemaFault> {
    if depth > MAX_DEPTH || *remaining == 0 {
        return Err(SchemaFault::Limit);
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                bounded(value, depth + 1, remaining)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                bounded(value, depth + 1, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn annotation(key: &str) -> bool {
    matches!(key, "$schema" | "title" | "description" | "default")
}
fn keys(value: &Value, allowed: &[&str]) -> Result<(), SchemaFault> {
    if value
        .get("$schema")
        .is_some_and(|v| v.as_str() != Some(DRAFT))
        || value.as_object().is_none_or(|v| {
            v.keys()
                .any(|k| !annotation(k) && !allowed.contains(&k.as_str()))
        })
    {
        return Err(SchemaFault::Shape);
    }
    Ok(())
}
fn definition<'a>(doc: &'a Value, name: &str) -> Result<&'a Value, SchemaFault> {
    doc.get("definitions")
        .and_then(|v| v.get(name))
        .ok_or(SchemaFault::Reference)
}
fn field<'a>(fields: &'a Map<String, Value>, name: &str) -> Result<&'a Value, SchemaFault> {
    fields.get(name).ok_or(SchemaFault::Shape)
}

fn object<'a>(
    value: &'a Value,
    required: &[&str],
    may_require: &[&str],
    fields: &[&str],
    allow_optional: bool,
) -> Result<&'a Map<String, Value>, SchemaFault> {
    keys(
        value,
        &[
            "type",
            "required",
            "properties",
            "definitions",
            "additionalProperties",
        ],
    )?;
    if value.get("type").and_then(Value::as_str) != Some("object")
        || value
            .get("additionalProperties")
            .is_some_and(|v| v != &Value::Bool(true))
    {
        return Err(SchemaFault::Shape);
    }
    let props = value
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(SchemaFault::Shape)?;
    if fields.iter().any(|k| !props.contains_key(*k))
        || (!allow_optional && props.keys().any(|k| !fields.contains(&k.as_str())))
    {
        return Err(SchemaFault::Shape);
    }
    let actual = match value.get("required") {
        None => BTreeSet::new(),
        Some(v) => string_set(v)?,
    };
    if required.iter().any(|k| !actual.contains(k))
        || actual
            .iter()
            .any(|k| !required.contains(k) && !may_require.contains(k))
    {
        return Err(SchemaFault::Shape);
    }
    Ok(props)
}

fn string_set(value: &Value) -> Result<BTreeSet<&str>, SchemaFault> {
    let values = value.as_array().ok_or(SchemaFault::Shape)?;
    let set = values
        .iter()
        .map(|v| v.as_str().ok_or(SchemaFault::Shape))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if set.len() != values.len() {
        return Err(SchemaFault::Shape);
    }
    Ok(set)
}

/// Only named local references are accepted; no resolver or recursive expansion.
fn reference(value: &Value, name: &str) -> Result<(), SchemaFault> {
    leaf(value, &json!({"$ref":format!("#/definitions/{name}")}))
        .map_err(|_| SchemaFault::Reference)
}
fn tag(value: &Value, tag: &str) -> Result<(), SchemaFault> {
    leaf(value, &json!({"type":"string","enum":[tag]}))
}
fn instructions(value: &Value) -> Result<(), SchemaFault> {
    leaf(value, &json!({"type":["string","null"]}))
}
fn array_ref(value: &Value, nullable: bool, name: &str) -> Result<(), SchemaFault> {
    let ty = if nullable {
        json!(["array", "null"])
    } else {
        json!("array")
    };
    leaf(
        value,
        &json!({"type":ty,"items":{"$ref":format!("#/definitions/{name}")}}),
    )
}

fn variant<'a>(
    value: &'a Value,
    discriminator: &str,
    wanted: &str,
) -> Result<&'a Value, SchemaFault> {
    tagged_variant(value, discriminator, wanted, false)
}

fn tagged_variant<'a>(
    value: &'a Value,
    discriminator: &str,
    wanted: &str,
    notification: bool,
) -> Result<&'a Value, SchemaFault> {
    let allowed: &[&str] = if notification {
        &["oneOf", "definitions", "properties"]
    } else {
        &["oneOf", "definitions"]
    };
    keys(value, allowed)?;
    if let Some(properties) = value.get("properties") {
        // Original ServerNotification adds this optional presentation field
        // outside the tagged union. It is not thread/turn or effect authority.
        let properties = properties.as_object().ok_or(SchemaFault::Shape)?;
        if properties.len() != 1 {
            return Err(SchemaFault::Shape);
        }
        leaf(
            field(properties, "emittedAtMs")?,
            &json!({"type":"integer","format":"int64"}),
        )?;
    }
    let branches = value
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or(SchemaFault::Shape)?;
    let mut names = BTreeSet::new();
    let mut selected = None;
    for branch in branches {
        // Every branch must be disjoint by a mandatory single string tag. An
        // untagged catch-all or duplicate could overlap the selected branch.
        // Draft-07 `$ref` would ignore sibling tag constraints. Other constraints
        // on an unselected tagged branch only narrow it; e.g. official 0.164's
        // imageGeneration branch adds anyOf while retaining its disjoint tag.
        // The selected branch is checked separately by the closed object recipe.
        if branch.get("$ref").is_some()
            || branch
                .get("$schema")
                .is_some_and(|v| v.as_str() != Some(DRAFT))
        {
            return Err(SchemaFault::Reference);
        }
        if branch.get("type").and_then(Value::as_str) != Some("object")
            || !string_set(branch.get("required").ok_or(SchemaFault::Shape)?)?
                .contains(discriminator)
        {
            return Err(SchemaFault::Shape);
        }
        let tag_schema = branch
            .get("properties")
            .and_then(|v| v.get(discriminator))
            .ok_or(SchemaFault::Shape)?;
        let tags = string_set(tag_schema.get("enum").ok_or(SchemaFault::Shape)?)?;
        let name = tags
            .first()
            .copied()
            .filter(|_| tags.len() == 1)
            .ok_or(SchemaFault::Shape)?;
        tag(tag_schema, name)?;
        if !names.insert(name) {
            return Err(SchemaFault::Shape);
        }
        if name == wanted {
            selected = Some(branch);
        }
    }
    selected.ok_or(SchemaFault::Shape)
}

#[derive(Clone, Copy)]
enum Direction {
    Incoming,
    Outgoing,
    Notification,
}
fn route(doc: &Value, method: &str, params: &str, direction: Direction) -> Result<(), SchemaFault> {
    let has_id = !matches!(direction, Direction::Notification);
    let names: &[&str] = if has_id {
        &["id", "method", "params"]
    } else {
        &["method", "params"]
    };
    let route = object(
        tagged_variant(doc, "method", method, !has_id)?,
        names,
        &[],
        names,
        matches!(direction, Direction::Outgoing),
    )?;
    tag(field(route, "method")?, method)?;
    reference(field(route, "params")?, params)?;
    if has_id {
        reference(field(route, "id")?, "RequestId")?;
    }
    Ok(())
}
fn input_text(value: &Value) -> Result<(), SchemaFault> {
    let fields = object(
        variant(value, "type", "inputText")?,
        &["type", "text"],
        &[],
        &["type", "text"],
        true,
    )?;
    tag(field(fields, "type")?, "inputText")?;
    leaf(field(fields, "text")?, &json!({"type":"string"}))
}
fn request_id(value: &Value) -> Result<(), SchemaFault> {
    leaf(
        value,
        &json!({"anyOf":[{"type":"string"},{"type":"integer","format":"int64"}]}),
    )
}

/// Structural equality for the small native-authored leaf recipes only. Arrays
/// are unordered sets; no input schema keyword is evaluated as executable logic.
fn leaf(actual: &Value, expected: &Value) -> Result<(), SchemaFault> {
    let equal = match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            a.get("$schema").is_none_or(|v| v.as_str() == Some(DRAFT))
                && a.keys().all(|k| annotation(k) || e.contains_key(k))
                && e.iter()
                    .all(|(k, v)| a.get(k).is_some_and(|x| leaf(x, v).is_ok()))
        }
        (Value::Array(a), Value::Array(e)) => {
            a.len() == e.len()
                && e.iter()
                    .all(|v| a.iter().filter(|x| leaf(x, v).is_ok()).count() == 1)
        }
        _ => actual == expected,
    };
    if equal {
        Ok(())
    } else {
        Err(SchemaFault::Shape)
    }
}

#[cfg(test)]
mod tests;
