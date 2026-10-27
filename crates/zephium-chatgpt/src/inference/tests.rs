use super::*;
use std::sync::Mutex;

fn request() -> WorkModelRequest {
    WorkModelRequest {
        image_fence: None,
        model: WorkModelRef {
            provider: WorkModelProvider::OpenAi,
            wire: WorkModelWire::OpenAiResponses,
            model: "gpt-fixture".into(),
        },
        system: vec![WorkModelSystemBlock {
            text: "Follow the user's request.".into(),
            cache: false,
        }],
        tools: vec![WorkModelTool {
            name: "read_page".into(),
            description: "Read one approved page".into(),
            schema: json!({"type":"object","properties":{},"additionalProperties":false}),
            strict: true,
        }],
        messages: vec![WorkModelMessage::User(vec![WorkModelPart::Text(
            "Hello".into(),
        )])],
        max_output_tokens: 2048,
        reasoning: None,
        native_search: false,
        parallel_tools: true,
        tool_choice: WorkModelToolChoice::Auto,
        volatile_tail: 0,
        cache_key: Some("ignored".into()),
    }
}

fn text_response() -> Value {
    json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]}],"usage":{"input_tokens":8,"output_tokens":2}}})
}

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

#[test]
fn exact_known_account_models_offer_existing_effort_slider_and_wire_values() {
    use WorkModelReasoning::*;
    for slug in [
        "gpt-6.1-sol",
        "gpt-6-luna",
        "gpt-6-sol",
        "gpt-6-astra",
        "gpt-5.6-terra",
    ] {
        let entry = Model {
            slug: slug.into(),
            display_name: "Clean model name".into(),
        }
        .entry();
        assert!(entry.supports.reasoning);
        assert_eq!(entry.display_name, "Clean model name");
        assert_eq!(entry.id, format!("chatgpt/{slug}"));
        for (effort, wire) in [
            (Low, "low"),
            (Medium, "medium"),
            (High, "high"),
            (XHigh, "xhigh"),
            (Max, "max"),
        ] {
            let mut request = request();
            request.model = entry.model.clone();
            request.reasoning = Some(effort);
            assert_eq!(
                request_body(&request).unwrap()["reasoning"],
                json!({"effort":wire})
            );
        }
    }
}

#[test]
fn unknown_aliases_and_similar_names_do_not_acquire_effort_capabilities() {
    for slug in [
        "gpt-6-unknown",
        "gpt-6.1-sol-custom",
        "gpt-5.6",
        "GPT-6-LUNA",
        "gpt-fixture",
    ] {
        let entry = Model {
            slug: slug.into(),
            display_name: "GPT-6 Luna".into(),
        }
        .entry();
        assert!(!entry.supports.reasoning);
        let mut request = request();
        request.model = entry.model;
        request.reasoning = Some(WorkModelReasoning::High);
        assert_eq!(
            request_body(&request).err(),
            Some(WorkModelError::BadRequest)
        );
    }
    assert!(request_body(&request()).unwrap().get("reasoning").is_none());
}

#[test]
fn preview_body_is_explicit_and_omits_incompatible_api_fields() {
    let body = request_body(&request()).unwrap();
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert!(body["input"].is_array());
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["tools"][0]["type"], "namespace");
    assert_eq!(body["tools"][0]["name"], "zephium");
    assert_eq!(body["tools"][0]["tools"][0]["type"], "function");
    for field in [
        "background",
        "conversation",
        "max_output_tokens",
        "max_tool_calls",
        "metadata",
        "moderation",
        "multi_agent",
        "prompt",
        "prompt_cache_retention",
        "safety_identifier",
        "temperature",
        "top_logprobs",
        "top_p",
        "truncation",
        "user",
        "previous_response_id",
    ] {
        assert!(body.get(field).is_none(), "{field}");
    }
    assert!(!body.to_string().contains("backend-api"));
}

#[test]
fn named_choice_offers_only_that_function_and_requires_its_use() {
    let mut request = request();
    let mut second = request.tools[0].clone();
    second.name = "other".into();
    request.tools.push(second);
    request.tool_choice = WorkModelToolChoice::Named("read_page".into());
    let body = request_body(&request).unwrap();
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(body["tools"][0]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["tools"][0]["name"], "read_page");
    let mut decoder = StreamDecoder::new(&request);
    assert_eq!(
        decoder
            .push(frame(text_response()).as_bytes(), &|_| {})
            .err(),
        Some(diagnostic(
            ChatGptFailureStage::Complete,
            Some(200),
            ChatGptFailureIssue::Protocol
        ))
    );
    let mut decoder = StreamDecoder::new(&request);
    let mut other = tool_response();
    other["response"]["output"][0]["name"] = json!("other");
    assert_eq!(
        decoder.push(frame(other).as_bytes(), &|_| {}).err(),
        Some(diagnostic(
            ChatGptFailureStage::Complete,
            Some(200),
            ChatGptFailureIssue::Protocol
        ))
    );
    request.tool_choice = WorkModelToolChoice::Named("not_offered".into());
    assert_eq!(
        request_body(&request).err(),
        Some(chatgpt_error(ChatGptModelFault::Unsupported))
    );
}

#[test]
fn only_a_completed_terminal_event_is_success() {
    let mut decoder = StreamDecoder::new(&request());
    let output = decoder
        .push(frame(text_response()).as_bytes(), &|_| {})
        .unwrap()
        .unwrap();
    assert_eq!(output.stop, WorkModelStop::EndTurn);
    assert_eq!(output.usage.input_tokens, 8);
    assert_eq!(output.usage.output_tokens, 2);
    assert!(matches!(&output.assistant[0], WorkModelPart::Text(text) if text == "Hello"));
    for (event, issue) in [
        (
            json!({"type":"response.incomplete","response":{"status":"incomplete"}}),
            ChatGptFailureIssue::Incomplete,
        ),
        (
            json!({"type":"response.failed","response":{"error":{"code":"unknown"}}}),
            ChatGptFailureIssue::Protocol,
        ),
    ] {
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder.push(frame(event).as_bytes(), &|_| {}).err(),
            Some(diagnostic(ChatGptFailureStage::Stream, Some(200), issue))
        );
    }
    let mut decoder = StreamDecoder::new(&request());
    assert!(decoder
        .push(
            frame(json!({"type":"response.output_text.delta","delta":"Partial"})).as_bytes(),
            &|_| {}
        )
        .unwrap()
        .is_none());
    assert_eq!(
        decoder.finish(&|_| {}).err(),
        Some(diagnostic(
            ChatGptFailureStage::Stream,
            Some(200),
            ChatGptFailureIssue::Incomplete
        ))
    );
    let mut decoder = StreamDecoder::new(&request());
    assert_eq!(
        decoder.push(b"data: [DONE]\n\n", &|_| {}).err(),
        Some(WorkModelError::Protocol)
    );
}

#[test]
fn chunks_crlf_unicode_and_eof_terminal_are_framed_without_duplicate_text() {
    let events = Mutex::new(Vec::new());
    let sink = |event| events.lock().unwrap().push(event);
    let mut decoder = StreamDecoder::new(&request());
    let delta = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hé\"}\r\n\r\n";
    for byte in delta.as_bytes() {
        assert!(decoder.push(&[*byte], &sink).unwrap().is_none());
    }
    let mut completed = text_response();
    completed["response"]["output"][0]["content"][0]["text"] = json!("Hé");
    let line = format!("event: response.completed\ndata: {completed}");
    assert!(decoder.push(line.as_bytes(), &sink).unwrap().is_none());
    let output = decoder.finish(&sink).unwrap();
    assert!(matches!(&output.assistant[0], WorkModelPart::Text(text) if text == "Hé"));
    assert_eq!(events.lock().unwrap().len(), 1);
}

#[test]
fn quota_failure_after_text_is_not_success_or_a_retryable_network_error() {
    for (code, expected) in [
        (
            "subscription_sharing_usage_limit_exceeded",
            chatgpt_error(ChatGptModelFault::UsageLimit),
        ),
        (
            "subscription_sharing_usage_unavailable",
            chatgpt_error(ChatGptModelFault::Unavailable),
        ),
    ] {
        let mut decoder = StreamDecoder::new(&request());
        decoder
            .push(
                frame(json!({"type":"response.output_text.delta","delta":"Partial"})).as_bytes(),
                &|_| {},
            )
            .unwrap();
        assert_eq!(decoder.push(frame(json!({"type":"response.failed","response":{"error":{"code":code,"message":"untrusted secret"}}})).as_bytes(), &|_| {}).err(), Some(diagnose(ChatGptFailureStage::Stream, Some(200), expected)));
    }
}

fn tool_response() -> Value {
    json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","call_id":"call_1","namespace":"zephium","name":"read_page","arguments":"{}"}],"usage":{"input_tokens":8,"output_tokens":3}}})
}

#[test]
fn tool_calls_are_validated_and_delivered_only_after_successful_terminal() {
    let events = Mutex::new(Vec::new());
    let sink = |event| events.lock().unwrap().push(event);
    let mut decoder = StreamDecoder::new(&request());
    decoder.push(frame(json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","namespace":"zephium","name":"read_page","arguments":"{}"}})).as_bytes(), &sink).unwrap();
    assert!(events.lock().unwrap().is_empty());
    let outcome = decoder
        .push(frame(tool_response()).as_bytes(), &sink)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.stop, WorkModelStop::ToolUse);
    assert_eq!(events.lock().unwrap().len(), 1);
    assert!(
        matches!(&outcome.assistant[0], WorkModelPart::ToolCall(call) if call.name == "read_page")
    );
}

#[test]
fn malformed_or_unoffered_tool_never_escapes_as_an_event() {
    for (key, value) in [
        ("namespace", json!("other")),
        ("name", json!("run_shell")),
        ("arguments", json!("not-json")),
        ("arguments", json!("[]")),
    ] {
        let mut response = tool_response();
        response["response"]["output"][0][key] = value;
        let events = Mutex::new(Vec::new());
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder
                .push(frame(response).as_bytes(), &|event| events
                    .lock()
                    .unwrap()
                    .push(event))
                .err(),
            Some(diagnostic(
                ChatGptFailureStage::Complete,
                Some(200),
                ChatGptFailureIssue::Protocol
            ))
        );
        assert!(events.lock().unwrap().is_empty());
    }
    let mut response = tool_response();
    let duplicate = response["response"]["output"][0].clone();
    response["response"]["output"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let mut decoder = StreamDecoder::new(&request());
    assert_eq!(
        decoder.push(frame(response).as_bytes(), &|_| {}).err(),
        Some(diagnostic(
            ChatGptFailureStage::Complete,
            Some(200),
            ChatGptFailureIssue::Protocol
        ))
    );
}

#[test]
fn a_late_invalid_item_cannot_release_an_earlier_tool_call() {
    let mut response = tool_response();
    response["response"]["output"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"hosted_tool_search","arguments":"{}"}));
    let events = Mutex::new(Vec::new());
    let mut decoder = StreamDecoder::new(&request());
    assert_eq!(
        decoder
            .push(frame(response).as_bytes(), &|event| events
                .lock()
                .unwrap()
                .push(event))
            .err(),
        Some(diagnostic(
            ChatGptFailureStage::Complete,
            Some(200),
            ChatGptFailureIssue::Protocol
        ))
    );
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn incomplete_terminal_data_and_invalid_accounting_are_not_success() {
    for (patch, issue) in [
        (json!({"status":"failed"}), ChatGptFailureIssue::Protocol),
        (json!({"usage":{}}), ChatGptFailureIssue::Protocol),
        (
            json!({"usage":{"input_tokens":1,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}}),
            ChatGptFailureIssue::Protocol,
        ),
    ] {
        let mut response = text_response();
        for (key, value) in patch.as_object().unwrap() {
            response["response"][key] = value.clone();
        }
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder.push(frame(response).as_bytes(), &|_| {}).err(),
            Some(diagnostic(ChatGptFailureStage::Complete, Some(200), issue))
        );
    }
}

#[test]
fn unsupported_images_search_and_cross_wire_replay_fail_before_dispatch() {
    let mut image = request();
    image.messages = vec![WorkModelMessage::User(vec![WorkModelPart::Image {
        media_type: "image/png".into(),
        bytes: vec![1, 2, 3],
    }])];
    assert_eq!(request_body(&image).err(), Some(WorkModelError::BadRequest));
    let mut search = request();
    search.native_search = true;
    assert_eq!(
        request_body(&search).err(),
        Some(WorkModelError::BadRequest)
    );
    let mut reasoning = request();
    reasoning.reasoning = Some(WorkModelReasoning::High);
    assert_eq!(
        request_body(&reasoning).err(),
        Some(WorkModelError::BadRequest)
    );
    let mut replay = request();
    replay.messages = vec![WorkModelMessage::Assistant(vec![WorkModelPart::Replay(
        json!({"wire":"openai","value":{"type":"reasoning"}}),
    )])];
    assert_eq!(
        request_body(&replay).err(),
        Some(WorkModelError::BadRequest)
    );
}

#[test]
fn history_replays_only_this_routes_bounded_reasoning_and_tool_results() {
    let mut request = request();
    request.messages.push(WorkModelMessage::Assistant(vec![WorkModelPart::ToolCall(WorkModelToolCall { id:"call_1".into(),name:"read_page".into(),arguments:json!({}) }), WorkModelPart::Replay(json!({"wire":"chatgpt","value":{"type":"reasoning","id":"r1","encrypted_content":"opaque","unexpected":"discarded"}}))]));
    request
        .messages
        .push(WorkModelMessage::ToolResults(vec![WorkModelToolResult {
            call: "call_1".into(),
            content: "result".into(),
            is_error: false,
        }]));
    let body = request_body(&request).unwrap();
    assert_eq!(body["input"][1]["namespace"], "zephium");
    assert_eq!(body["input"][2]["encrypted_content"], "opaque");
    assert!(body["input"][2].get("unexpected").is_none());
    assert_eq!(body["input"][3]["type"], "function_call_output");
}

#[test]
fn stream_bounds_apply_before_json_and_partial_utf8_is_rejected() {
    let mut decoder = StreamDecoder::new(&request());
    assert_eq!(
        decoder
            .push(&vec![b'x'; MAX_EVENT_BYTES + 1], &|_| {})
            .err(),
        Some(WorkModelError::Protocol)
    );
    let mut decoder = StreamDecoder::new(&request());
    assert_eq!(
        decoder.push(b"data: \xff\n\n", &|_| {}).err(),
        Some(WorkModelError::Protocol)
    );
    let mut decoder = StreamDecoder::new(&request());
    decoder.bytes = MAX_STREAM_BYTES;
    assert_eq!(
        decoder.push(b"x", &|_| {}).err(),
        Some(WorkModelError::Protocol)
    );
}

#[test]
fn public_preflight_is_exact_and_content_free() {
    let mut request = request();
    request.model.model = "gpt-6.1-sol".into();
    request.reasoning = Some(WorkModelReasoning::High);
    request.messages = vec![WorkModelMessage::User(vec![WorkModelPart::Text(
        "مرحبا".into(),
    )])];
    assert_eq!(encode_request(&request).map(|_| ()), Ok(()));
    let payload: Value = serde_json::from_slice(&encode_request(&request).unwrap()).unwrap();
    assert_eq!(payload["model"], "gpt-6.1-sol");
    assert_eq!(payload["reasoning"]["effort"], "high");
    assert_eq!(payload["store"], false);
    assert_eq!(payload["stream"], true);
    assert_eq!(payload["tools"][0]["type"], "namespace");
    request.tools[0].description = "PRIVATE_REQUEST_SENTINEL".repeat(4096);
    let error = encode_request(&request).map(|_| ()).unwrap_err();
    assert_eq!(
        error,
        diagnostic(
            ChatGptFailureStage::Request,
            None,
            ChatGptFailureIssue::InvalidRequest
        )
    );
    assert!(!format!("{error:?}").contains("PRIVATE_REQUEST_SENTINEL"));
}

#[test]
fn missing_or_null_usage_retains_valid_tools_but_never_emits_tool_callbacks() {
    for missing in [true, false] {
        for choice in [
            WorkModelToolChoice::Auto,
            WorkModelToolChoice::Named("read_page".into()),
        ] {
            let mut response = tool_response();
            if missing {
                response["response"]
                    .as_object_mut()
                    .unwrap()
                    .remove("usage");
            } else {
                response["response"]["usage"] = Value::Null;
            }
            let mut request = request();
            request.tool_choice = choice;
            let events = Mutex::new(Vec::new());
            let outcome = StreamDecoder::new(&request)
                .push(frame(response).as_bytes(), &|event| {
                    events.lock().unwrap().push(event)
                })
                .unwrap()
                .unwrap();
            assert_eq!(outcome.stop, WorkModelStop::ToolUse);
            assert_eq!(outcome.usage.status, WorkModelUsageStatus::Unavailable);
            assert_eq!(outcome.usage.cost_micros, None);
            assert!(
                matches!(&outcome.assistant[..], [WorkModelPart::ToolCall(call)]
                    if call.id == "call_1" && call.name == "read_page" && call.arguments == json!({}))
            );
            assert!(events.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn missing_or_null_usage_does_not_change_valid_refusal_empty_or_mixed_output() {
    for missing in [true, false] {
        for shape in 0..3 {
            let mut response = text_response();
            response["response"]["output"] = match shape {
                0 => json!([]),
                1 => {
                    json!([{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot help with that."}]}])
                }
                _ => json!([
                    {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Checking the page."}]},
                    {"type":"function_call","call_id":"call_1","namespace":"zephium","name":"read_page","arguments":"{}"}
                ]),
            };
            if missing {
                response["response"]
                    .as_object_mut()
                    .unwrap()
                    .remove("usage");
            } else {
                response["response"]["usage"] = Value::Null;
            }
            let events = Mutex::new(Vec::new());
            let outcome = StreamDecoder::new(&request())
                .push(frame(response).as_bytes(), &|event| {
                    events.lock().unwrap().push(event)
                })
                .unwrap()
                .unwrap();
            assert_eq!(
                outcome.stop,
                match shape {
                    0 => WorkModelStop::EndTurn,
                    1 => WorkModelStop::Refused,
                    _ => WorkModelStop::ToolUse,
                }
            );
            assert_eq!(outcome.usage.status, WorkModelUsageStatus::Unavailable);
            assert_eq!(outcome.usage.cost_micros, None);
            let events = events.lock().unwrap();
            assert!(events
                .iter()
                .all(|event| matches!(event, WorkModelEvent::Text(_))));
            assert_eq!(events.len(), usize::from(shape == 2));
        }
    }
}

#[test]
fn absent_usage_never_relaxes_terminal_or_tool_validation() {
    for missing in [true, false] {
        for invalid in 0..5 {
            let mut response = tool_response();
            if missing {
                response["response"]
                    .as_object_mut()
                    .unwrap()
                    .remove("usage");
            } else {
                response["response"]["usage"] = Value::Null;
            }
            match invalid {
                0 => response["response"]["status"] = json!("incomplete"),
                1 => response["response"]["output"][0]["namespace"] = json!("other"),
                2 => response["response"]["output"][0]["name"] = json!("not_offered"),
                3 => response["response"]["output"][0]["arguments"] = json!("[]"),
                _ => response["response"]["output"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"unknown"})),
            }
            let events = Mutex::new(Vec::new());
            let error = StreamDecoder::new(&request())
                .push(frame(response).as_bytes(), &|event| {
                    events.lock().unwrap().push(event)
                })
                .err()
                .unwrap();
            assert_eq!(
                error,
                diagnostic(
                    ChatGptFailureStage::Complete,
                    Some(200),
                    ChatGptFailureIssue::Protocol
                )
            );
            assert!(events.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn completed_plain_text_accepts_missing_or_null_usage_as_unknown_not_zero() {
    for missing in [true, false] {
        for reasoning in [true, false] {
            let mut response = text_response();
            if missing {
                response["response"]
                    .as_object_mut()
                    .unwrap()
                    .remove("usage");
            } else {
                response["response"]["usage"] = Value::Null;
            }
            if reasoning {
                response["response"]["output"].as_array_mut().unwrap().insert(0,
                        json!({"type":"reasoning","id":"rs_fixture","summary":[],"encrypted_content":"fixture-encrypted"}));
            }
            let events = Mutex::new(Vec::new());
            let outcome = StreamDecoder::new(&request())
                .push(frame(response).as_bytes(), &|event| {
                    events.lock().unwrap().push(event)
                })
                .unwrap()
                .unwrap();
            assert_eq!(outcome.stop, WorkModelStop::EndTurn);
            assert_eq!(outcome.usage.status, WorkModelUsageStatus::Unavailable);
            assert!(!outcome.usage.is_reported());
            assert_eq!(outcome.usage.cost_micros, None);
            assert!(outcome
                .assistant
                .iter()
                .any(|part| matches!(part, WorkModelPart::Text(text) if text == "Hello")));
            assert!(events
                .lock()
                .unwrap()
                .iter()
                .all(|event| matches!(event, WorkModelEvent::Text(_))));
        }
    }
}

#[test]
fn explicit_zero_is_reported_but_malformed_present_usage_is_not_absence() {
    let zero = json!({"input_tokens":0,"output_tokens":0,"total_tokens":0});
    let reported = response_usage(Some(&zero)).unwrap().unwrap();
    assert!(reported.is_reported());
    assert_eq!(reported.input_tokens, 0);
    for value in [
        json!({}),
        json!("0"),
        json!({"input_tokens":"1","output_tokens":2}),
        json!({"input_tokens":1,"output_tokens":-1}),
        json!({"input_tokens":1,"output_tokens":2,"total_tokens":4}),
        json!({"input_tokens":1,"output_tokens":2,"input_tokens_details":"bad"}),
        json!({"input_tokens":1,"output_tokens":2,"output_tokens_details":{"reasoning_tokens":"2"}}),
        json!({"input_tokens":u64::MAX,"output_tokens":1}),
    ] {
        assert_eq!(response_usage(Some(&value)), Err(WorkModelError::Protocol));
    }
    let mut response = tool_response();
    response["response"]["usage"] = json!({"input_tokens":"not-a-count","output_tokens":1});
    let events = Mutex::new(Vec::new());
    assert!(StreamDecoder::new(&request())
        .push(frame(response).as_bytes(), &|event| events
            .lock()
            .unwrap()
            .push(event))
        .is_err());
    assert!(events.lock().unwrap().is_empty());
}
