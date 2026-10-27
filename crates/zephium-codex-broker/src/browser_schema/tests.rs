use super::*;

// Compact reachable fragments of the official 0.163/0.164 experimental archive;
// the private qualification additionally reads both complete original exports.
const FIXTURE: &str = include_str!("fixtures/browser-v1.json");
const KEYS: [&str; 5] = [
    "server_requests",
    "tool_response",
    "rpc_response",
    "client_requests",
    "server_notifications",
];

fn documents() -> [Value; 5] {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    KEYS.map(|key| fixture[key].clone())
}
fn bytes(docs: &[Value; 5]) -> [Vec<u8>; 5] {
    docs.each_ref().map(|v| serde_json::to_vec(v).unwrap())
}
fn input(data: &[Vec<u8>; 5]) -> BrowserSchemas<'_> {
    BrowserSchemas {
        server_requests: &data[0],
        tool_response: &data[1],
        rpc_response: &data[2],
        client_requests: &data[3],
        server_notifications: &data[4],
    }
}
fn check(docs: &[Value; 5]) -> Result<BrowserShape, SchemaFault> {
    project(input(&bytes(docs)))
}
fn change(index: usize, pointer: &str, value: Value) -> [Value; 5] {
    let mut docs = documents();
    *docs[index].pointer_mut(pointer).unwrap() = value;
    docs
}

#[test]
fn admits_reachable_experimental_browser_contract_not_an_execution_permit() {
    let shape = check(&documents()).unwrap();
    assert_ne!(shape.digest(), [0; 32]);
    assert_eq!(FILE_NAMES.len(), KEYS.len());
}

#[test]
fn stable_or_orphan_definitions_do_not_admit_registration() {
    let mut docs = documents();
    docs[3]["definitions"]["ThreadStartParams"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("dynamicTools");
    assert!(docs[3]["definitions"]["DynamicToolSpec"].is_object());
    assert_eq!(check(&docs), Err(SchemaFault::Shape));
    let mut docs = documents();
    docs[3]["oneOf"].as_array_mut().unwrap().remove(0);
    assert!(check(&docs).is_err());
}

#[test]
fn each_request_correlation_field_is_mandatory() {
    for name in ["arguments", "callId", "threadId", "tool", "turnId"] {
        let mut docs = documents();
        docs[0]["definitions"]["DynamicToolCallParams"]["required"]
            .as_array_mut()
            .unwrap()
            .retain(|v| v != name);
        assert_eq!(check(&docs), Err(SchemaFault::Shape), "{name}");
    }
    for name in ["id", "method", "params"] {
        let mut docs = documents();
        docs[0]["oneOf"][0]["required"]
            .as_array_mut()
            .unwrap()
            .retain(|v| v != name);
        assert!(check(&docs).is_err(), "{name}");
    }
}

#[test]
fn request_identity_domain_cannot_widen_or_narrow() {
    for definition in [
        json!({"type":"string"}),
        json!({"type":"number"}),
        json!({"anyOf":[{"type":"string"},{"type":"integer","format":"uint64"}]}),
        json!({"anyOf":[{"type":"string"},{"type":"integer","format":"int64","minimum":0}]}),
        json!({"anyOf":[{"type":"string"},{"type":"integer","format":"int64"},{"type":"null"}]}),
    ] {
        for index in [0, 2, 3] {
            assert!(check(&change(index, "/definitions/RequestId", definition.clone())).is_err());
        }
    }
}

#[test]
fn declared_namespace_is_optional_but_incoming_shape_remains_closed() {
    let mut docs = documents();
    docs[0]["definitions"]["DynamicToolCallParams"]["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("namespace"));
    assert!(check(&docs).is_err());
    let mut docs = documents();
    docs[0]["definitions"]["DynamicToolCallParams"]["properties"]["newAuthority"] =
        json!({"type":"string"});
    assert!(check(&docs).is_err());
    let mut docs = documents();
    docs[0]["oneOf"][0]["properties"]["trace"] = json!({"type":"string"});
    assert!(check(&docs).is_err());
}

#[test]
fn output_must_support_boolean_success_and_input_text() {
    for (index, pointer, replacement) in [
        (1, "/properties/success", json!({"type":"string"})),
        (1, "/required", json!(["contentItems"])),
        (
            1,
            "/definitions/DynamicToolCallOutputContentItem/oneOf/0/properties/text",
            json!({"type":"number"}),
        ),
        (
            1,
            "/definitions/DynamicToolCallOutputContentItem/oneOf/0/required",
            json!(["type"]),
        ),
        (2, "/required", json!(["result"])),
        (2, "/properties/result", json!({"type":"object"})),
    ] {
        assert!(
            check(&change(index, pointer, replacement)).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn optional_outgoing_declarations_are_additive_not_authority() {
    let expected = check(&documents()).unwrap();
    let mut docs = documents();
    docs[3]["definitions"]["ThreadResumeParams"]["properties"]["dynamicTools"] =
        json!({"type":["array","null"],"items":true});
    docs[3]["definitions"]["ThreadStartParams"]["properties"]["futurePresentation"] =
        json!({"type":"string"});
    assert_eq!(check(&docs).unwrap(), expected);
    docs[3]["definitions"]["ThreadResumeParams"]["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("dynamicTools"));
    assert!(check(&docs).is_err());
}

#[test]
fn optional_outgoing_response_and_request_envelope_fields_are_not_sent() {
    let expected = check(&documents()).unwrap();
    for (index, pointer) in [(1, ""), (2, ""), (3, "/oneOf/0"), (3, "/oneOf/1")] {
        let mut docs = documents();
        let envelope = docs[index].pointer_mut(pointer).unwrap();
        envelope["properties"]["futureOptional"] = json!({"type":"string"});
        assert_eq!(check(&docs).unwrap(), expected, "{index}:{pointer}");
        docs[index].pointer_mut(pointer).unwrap()["required"]
            .as_array_mut()
            .unwrap()
            .push(json!("futureOptional"));
        assert!(check(&docs).is_err(), "{index}:{pointer}");
    }
}

#[test]
fn newly_required_unsent_fields_are_not_ignored() {
    for (index, pointer) in [
        (1, "/required"),
        (3, "/definitions/ThreadResumeParams/required"),
        (3, "/definitions/DynamicToolSpec/oneOf/0/required"),
        (
            1,
            "/definitions/DynamicToolCallOutputContentItem/oneOf/0/required",
        ),
    ] {
        let mut docs = documents();
        docs[index]
            .pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!("unsent"));
        assert!(check(&docs).is_err(), "{pointer}");
    }
    let mut docs = documents();
    docs[3]["definitions"]["ThreadStartParams"]["required"] = json!(["unsent"]);
    assert!(check(&docs).is_err());
}

#[test]
fn explicitly_supplied_optional_fields_may_become_required() {
    let expected = check(&documents()).unwrap();
    let mut docs = documents();
    docs[3]["definitions"]["ThreadStartParams"]["required"] =
        json!(["dynamicTools", "developerInstructions"]);
    docs[3]["definitions"]["ThreadResumeParams"]["required"] =
        json!(["threadId", "developerInstructions"]);
    docs[3]["definitions"]["DynamicToolSpec"]["oneOf"][0]["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("deferLoading"));
    assert_eq!(check(&docs).unwrap(), expected);
}

#[test]
fn function_and_instruction_shapes_are_not_replaced_by_names() {
    for (pointer, replacement) in [
        (
            "/definitions/ThreadStartParams/properties/dynamicTools/items",
            json!({"$ref":"#/definitions/Other"}),
        ),
        (
            "/definitions/ThreadResumeParams/properties/developerInstructions",
            json!({"type":"boolean"}),
        ),
        (
            "/definitions/DynamicToolSpec/oneOf/0/properties/inputSchema",
            json!({"type":"object"}),
        ),
        (
            "/definitions/DynamicToolSpec/oneOf/0/properties/deferLoading",
            json!({"const":true}),
        ),
        (
            "/definitions/DynamicToolSpec/oneOf/0/required",
            json!(["type", "name", "inputSchema"]),
        ),
    ] {
        assert!(
            check(&change(3, pointer, replacement)).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn lifecycle_requires_scope_timestamps_and_supported_statuses() {
    for (pointer, replacement) in [
        (
            "/definitions/ItemStartedNotification/required",
            json!(["item", "threadId", "startedAtMs"]),
        ),
        (
            "/definitions/ItemCompletedNotification/properties/completedAtMs",
            json!({"type":"number"}),
        ),
        (
            "/definitions/ThreadItem/oneOf/0/properties/id",
            json!({"type":"integer"}),
        ),
        (
            "/definitions/ThreadItem/oneOf/0/properties/success",
            json!({"type":"boolean"}),
        ),
        (
            "/definitions/DynamicToolCallStatus/enum",
            json!(["inProgress", "completed", "failed", "executed"]),
        ),
    ] {
        assert!(
            check(&change(4, pointer, replacement)).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn references_are_exact_local_names_not_a_resolver() {
    for reference in [
        "https://example.invalid/schema",
        "other.json#/definitions/RequestId",
        "#/definitions/Unknown",
        "#/definitions/RequestId/../RequestId",
    ] {
        assert_eq!(
            check(&change(
                0,
                "/oneOf/0/properties/id",
                json!({"$ref":reference})
            )),
            Err(SchemaFault::Reference)
        );
    }
    let mut docs = documents();
    docs[0]["definitions"]["RequestId"] = json!({"$ref":"#/definitions/RequestId"});
    assert!(check(&docs).is_err());
    let mut docs = documents();
    docs[0]["definitions"]
        .as_object_mut()
        .unwrap()
        .remove("DynamicToolCallParams");
    assert_eq!(check(&docs), Err(SchemaFault::Reference));
}

#[test]
fn ambiguous_discriminants_and_ref_shadowing_fail_closed() {
    for index in [0, 3, 4] {
        let mut docs = documents();
        let duplicate = docs[index]["oneOf"][0].clone();
        docs[index]["oneOf"].as_array_mut().unwrap().push(duplicate);
        assert!(check(&docs).is_err());
        let mut docs = documents();
        docs[index]["oneOf"]
            .as_array_mut()
            .unwrap()
            .push(json!(true));
        assert!(check(&docs).is_err());
    }
    let mut docs = documents();
    let mut unrelated = docs[0]["oneOf"][0].clone();
    unrelated["properties"]["method"]["enum"] = json!(["unrelated"]);
    unrelated["$ref"] = json!("#/definitions/DynamicToolCallParams");
    docs[0]["oneOf"].as_array_mut().unwrap().push(unrelated);
    assert!(check(&docs).is_err());
}

#[test]
fn documentation_order_and_disjoint_unused_routes_do_not_change_contract() {
    let expected = check(&documents()).unwrap();
    let mut docs = documents();
    docs[0]["title"] = json!("new label");
    docs[0]["description"] = json!("new documentation");
    docs[0]["definitions"]["RequestId"]["anyOf"]
        .as_array_mut()
        .unwrap()
        .reverse();
    docs[0]["definitions"]["DynamicToolCallParams"]["required"]
        .as_array_mut()
        .unwrap()
        .reverse();
    docs[3]["definitions"]["ThreadStartParams"]["properties"]["dynamicTools"]["type"]
        .as_array_mut()
        .unwrap()
        .reverse();
    docs[3]["definitions"]["Unrelated"] = json!({"allOf":[false,true]});
    let mut unrelated = docs[0]["oneOf"][0].clone();
    unrelated["properties"]["method"]["enum"] = json!(["future/unrelated"]);
    docs[0]["oneOf"].as_array_mut().unwrap().push(unrelated);
    assert_eq!(check(&docs).unwrap(), expected);
}

#[test]
fn disjoint_unselected_branch_may_have_additional_constraints() {
    let expected = check(&documents()).unwrap();
    let mut docs = documents();
    // The real 0.164 imageGeneration variant adds an anyOf requirement but its
    // outer mandatory singleton type still proves it cannot match dynamicToolCall.
    let unrelated = json!({
        "type":"object", "required":["type"],
        "properties":{"type":{"type":"string","enum":["imageGeneration"]}},
        "anyOf":[{"required":["fileId"]},{"required":["result"]}]
    });
    docs[4]["definitions"]["ThreadItem"]["oneOf"]
        .as_array_mut()
        .unwrap()
        .push(unrelated);
    assert_eq!(check(&docs).unwrap(), expected);
    docs[4]["definitions"]["ThreadItem"]["oneOf"][1]["$ref"] = json!("#/definitions/ThreadItem");
    assert_eq!(check(&docs), Err(SchemaFault::Reference));
}

#[test]
fn original_outer_notification_timestamp_is_optional_presentation_only() {
    let expected = check(&documents()).unwrap();
    let mut docs = documents();
    docs[4].as_object_mut().unwrap().remove("properties");
    assert_eq!(check(&docs).unwrap(), expected);
    docs[4]["properties"] = json!({"emittedAtMs":{"type":"integer","format":"int64"}});
    assert_eq!(check(&docs).unwrap(), expected);
    docs[4]["required"] = json!(["emittedAtMs"]);
    assert!(check(&docs).is_err());
    let mut docs = documents();
    docs[4]["properties"]["newAuthority"] = json!({"type":"string"});
    assert!(check(&docs).is_err());
    let mut docs = documents();
    docs[4]["properties"]["emittedAtMs"] = json!({"type":"number"});
    assert!(check(&docs).is_err());
}

#[test]
fn unknown_constraints_or_dialects_are_not_evaluated() {
    for (index, pointer, keyword, value) in [
        (
            0,
            "/definitions/DynamicToolCallParams",
            "allOf",
            json!([true]),
        ),
        (
            3,
            "/definitions/ThreadStartParams",
            "additionalProperties",
            json!(false),
        ),
        (
            0,
            "/definitions/DynamicToolCallParams/properties/callId",
            "minLength",
            json!(1),
        ),
        (
            3,
            "/definitions/ThreadStartParams/properties/dynamicTools",
            "maxItems",
            json!(8),
        ),
        (
            0,
            "/definitions/DynamicToolCallParams/properties/callId",
            "$schema",
            json!("future-draft"),
        ),
    ] {
        let mut docs = documents();
        docs[index]
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(keyword.into(), value);
        assert!(check(&docs).is_err(), "{keyword}");
    }
    assert!(check(&change(0, "/$schema", json!("future-draft"))).is_err());
}

#[test]
fn duplicate_json_keys_and_duplicate_sets_are_rejected() {
    let mut data = bytes(&documents());
    data[0] = br#"{"$schema":"x","$schema":"y"}"#.to_vec();
    assert_eq!(project(input(&data)), Err(SchemaFault::Json));
    let mut data = bytes(&documents());
    let text = String::from_utf8(data[0].clone()).unwrap();
    data[0] = text
        .replacen("\"callId\":{", "\"callId\":true,\"callId\":{", 1)
        .into_bytes();
    assert_eq!(project(input(&data)), Err(SchemaFault::Json));
    let mut docs = documents();
    docs[0]["definitions"]["DynamicToolCallParams"]["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("callId"));
    assert!(check(&docs).is_err());
}

#[test]
fn bounded_input_is_checked_before_shape_admission() {
    for data in [Vec::new(), vec![b' '; MAX_FILE_BYTES + 1]] {
        let mut files = bytes(&documents());
        files[0] = data;
        assert_eq!(project(input(&files)), Err(SchemaFault::Limit));
    }
    let mut docs = documents();
    let mut nested = json!(null);
    for _ in 0..MAX_DEPTH + 1 {
        nested = json!([nested]);
    }
    docs[0]["unused"] = nested;
    assert_eq!(check(&docs), Err(SchemaFault::Limit));
    let mut docs = documents();
    docs[0]["unused"] = json!(vec![0; MAX_NODES]);
    assert_eq!(check(&docs), Err(SchemaFault::Limit));
}

#[test]
#[ignore = "private exact official experimental archive fixtures; never invokes the CLI"]
fn complete_official_163_and_164_exports_match() {
    let root =
        std::env::var_os("ZEPHIUM_BROWSER_SCHEMA_FIXTURES").expect("private source fixture root");
    let root = std::path::PathBuf::from(root);
    let mut shapes = Vec::new();
    for version in ["163", "164"] {
        let files = FILE_NAMES
            .map(|name| std::fs::read(root.join(version).join("json").join(name)).unwrap());
        let result = project(input(&files));
        if result.is_err() {
            // Diagnose the exact public document, without dumping schema bodies
            // or weakening the production fault projection.
            for index in 0..files.len() {
                let mut isolated = bytes(&documents());
                isolated[index] = files[index].clone();
                assert!(
                    project(input(&isolated)).is_ok(),
                    "original {version}: {} projection failed",
                    FILE_NAMES[index]
                );
            }
        }
        shapes.push(result.unwrap());
    }
    assert_eq!(shapes[0], shapes[1]);
}
