use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;
const SCOPE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn session() -> SessionId {
    SessionId::parse("b".repeat(64)).unwrap()
}
fn request(wire: WorkModelWire) -> WorkModelRequest {
    WorkModelRequest {
        image_fence: None,
        model: WorkModelRef {
            provider: WorkModelProvider::OpenCodeGo,
            wire,
            model: if wire == WorkModelWire::OpenAiResponses {
                "gpt-6-luna"
            } else {
                "kimi-k3"
            }
            .into(),
        },
        system: vec![WorkModelSystemBlock {
            text: "Use offered tools.".into(),
            cache: false,
        }],
        tools: vec![WorkModelTool {
            name: "read_file".into(),
            description: "Read an approved file.".into(),
            schema: json!({"type":"object","properties":{},"additionalProperties":false}),
            strict: false,
        }],
        messages: vec![WorkModelMessage::User(vec![WorkModelPart::Text(
            "Synthetic coding fixture.".into(),
        )])],
        max_output_tokens: 128,
        reasoning: None,
        native_search: false,
        parallel_tools: true,
        tool_choice: WorkModelToolChoice::Auto,
        volatile_tail: 0,
        cache_key: None,
    }
}
fn client() -> OpenCodeGoClient {
    OpenCodeGoClient::new(
        Arc::new(LeadSecret::new("fixture-go-key".into()).unwrap()),
        SCOPE.into(),
    )
    .unwrap()
}
fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}
fn choice(delta: Value, finish: Option<&str>) -> Value {
    json!({"object":"chat.completion.chunk","id":"fixture-response","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
}
fn usage(wire: WorkModelWire) -> Value {
    let mut value = json!({"total_tokens":12,"cost":0,"opencode":{"billing_source":"go","list_cost":0.00012,"allowance":0.00006}});
    if wire == WorkModelWire::ChatCompletions {
        value["prompt_tokens"] = json!(10);
        value["completion_tokens"] = json!(2);
    } else {
        value["input_tokens"] = json!(10);
        value["output_tokens"] = json!(2);
    }
    value
}
fn chat_end(value: Value) -> String {
    format!(
        "{}{}data: [DONE]\n\n",
        frame(choice(json!({}), Some("stop"))),
        frame(json!({"choices":[],"usage":value}))
    )
}
fn chat_text() -> String {
    format!(
        "{}{}",
        frame(choice(json!({"content":"Hello"}), None)),
        chat_end(usage(WorkModelWire::ChatCompletions))
    )
}
fn completed(output: Value, usage: Value) -> Value {
    json!({"type":"response.completed","response":{"id":"fixture-response","status":"completed","output":output,"usage":usage}})
}
fn response_text() -> Value {
    completed(
        json!([{"type":"message","id":"message1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Hello"}]}]),
        usage(WorkModelWire::OpenAiResponses),
    )
}
fn decode(wire: WorkModelWire, stream: &str) -> Result<GoOutcome, GoFailure> {
    let mut d = GoStream::new(&request(wire), &session(), SCOPE);
    d.push(stream.as_bytes(), &|_| {})?
        .ok_or_else(|| d.failure(GoFault::Incomplete))
}
#[test]
fn exact_catalog_intersection_does_not_guess_or_enable_training_models() {
    let models=parse_models(br#"{"object":"list","data":[{"id":"gpt-6-luna"},{"id":"kimi-k3"},{"id":"minimax-m3"},{"id":"gemini-4-argon"},{"id":"muse-spark-1.3-contributor"},{"id":"gpt-unknown"}]}"#).unwrap();
    assert_eq!(models.supported.len(), 2);
    assert_eq!(models.supported[0].wire, WorkModelWire::OpenAiResponses);
    assert_eq!(models.supported[1].wire, WorkModelWire::ChatCompletions);
    assert_eq!(
        models
            .unsupported
            .iter()
            .map(|m| m.reason)
            .collect::<Vec<_>>(),
        vec![
            UnsupportedReason::ProtocolNotImplemented,
            UnsupportedReason::ProtocolNotImplemented,
            UnsupportedReason::TrainingDisclosureRequired,
            UnsupportedReason::UnknownModel
        ]
    );
    for bytes in [
        br#"{"data":[{"id":"same"},{"id":"same"}]}"#.as_slice(),
        br#"{"data":[{"id":"bad\nname"}]}"#.as_slice(),
        br#"{"models":[]}"#.as_slice(),
    ] {
        assert_eq!(parse_models(bytes).err(), Some(GoFault::Protocol));
    }
    assert_eq!(
        parse_models(&vec![b' '; MAX_CATALOG_BYTES + 1]).err(),
        Some(GoFault::TooLarge)
    );
}
#[test]
fn fixed_origin_credentials_and_native_opaque_session_are_validated() {
    let c = client();
    assert_eq!(
        c.endpoint("v1/models"),
        "https://opencode.ai/inference/go/v1/models"
    );
    for invalid in [
        "",
        "user text",
        &"A".repeat(64),
        &"b".repeat(63),
        &"b".repeat(65),
    ] {
        assert!(SessionId::parse(invalid.into()).is_err());
    }
    assert!(!format!("{:?}", session()).contains(&"b".repeat(64)));
    assert!(OpenCodeGoClient::new(c.key.clone(), "not-scope".into()).is_err());
    assert!(OpenCodeGoClient::new(
        Arc::new(LeadSecret::new("x".repeat(1025)).unwrap()),
        SCOPE.into()
    )
    .is_err());
}
#[test]
fn request_uses_exact_supported_wire_and_does_not_repurpose_cache_key() {
    let target = LeadTarget::compatible("https://opencode.ai/inference/go/openai/v1").unwrap();
    for wire in [
        WorkModelWire::ChatCompletions,
        WorkModelWire::OpenAiResponses,
    ] {
        let mut req = request(wire);
        req.cache_key = Some("cache-is-not-a-conversation".into());
        let body: Value =
            serde_json::from_slice(&request_body(&req, &target, &session(), SCOPE).unwrap())
                .unwrap();
        assert_eq!(body["stream"], true);
        assert!(body.get("prompt_cache_key").is_none());
        if wire == WorkModelWire::OpenAiResponses {
            assert_eq!(body["store"], false);
            assert!(body.get("previous_response_id").is_none());
        } else {
            assert_eq!(body["stream_options"]["include_usage"], true);
        }
        req.model.model = "gpt-future".into();
        assert_eq!(
            request_body(&req, &target, &session(), SCOPE).err(),
            Some(GoFault::InvalidRequest)
        );
    }
    let mut req = request(WorkModelWire::ChatCompletions);
    req.model.model = "gpt-6-luna".into();
    assert!(request_body(&req, &target, &session(), SCOPE).is_err());
}
#[test]
fn chat_terminal_requires_usage_and_known_billing_without_false_quota() {
    let result = decode(WorkModelWire::ChatCompletions, &chat_text()).unwrap();
    assert_eq!(result.outcome.usage.input_tokens, 10);
    assert_eq!(result.outcome.usage.cost_micros, None);
    assert_eq!(result.billing.source, BillingSource::Go);
    assert_eq!(
        result.billing.allowance_consumed_usd.unwrap().as_str(),
        "0.00006"
    );
    for bad in [
        json!(null),
        json!({"prompt_tokens":10,"completion_tokens":2}),
        json!({"prompt_tokens":10,"completion_tokens":2,"opencode":{"billing_source":"unknown"}}),
    ] {
        let stream = format!(
            "{}{}",
            frame(choice(json!({"content":"Hello"}), None)),
            chat_end(bad)
        );
        assert!(decode(WorkModelWire::ChatCompletions, &stream).is_err());
    }
    for stream in [
        "data: [DONE]\n\n".into(),
        frame(choice(json!({"content":"Hello"}), Some("stop"))),
        format!(
            "{}data: [DONE]\n\n",
            frame(choice(json!({"content":"Hi"}), Some("length")))
        ),
    ] {
        assert!(decode(WorkModelWire::ChatCompletions, &stream).is_err());
    }
}
#[test]
fn responses_require_complete_authoritative_usage_and_matching_deltas() {
    let delta = frame(
        json!({"type":"response.output_text.delta","response_id":"fixture-response","output_index":0,"content_index":0,"delta":"Hello"}),
    );
    let result = decode(
        WorkModelWire::OpenAiResponses,
        &format!("{delta}{}", frame(response_text())),
    )
    .unwrap();
    assert_eq!(result.outcome.stop, WorkModelStop::EndTurn);
    for kind in ["response.incomplete", "response.failed", "unknown.event"] {
        assert!(decode(WorkModelWire::OpenAiResponses, &frame(json!({"type":kind}))).is_err());
    }
    assert!(decode(
        WorkModelWire::OpenAiResponses,
        &format!(
            "{}{}",
            delta.replace("Hello", "Wrong"),
            frame(response_text())
        )
    )
    .is_err());
    let mut final_value = response_text();
    final_value["response"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("opencode");
    assert_eq!(
        decode(WorkModelWire::OpenAiResponses, &frame(final_value))
            .err()
            .unwrap()
            .fault,
        GoFault::UsageUnavailable
    );
    assert!(decode(WorkModelWire::OpenAiResponses, "data: [DONE]\n\n").is_err());
}
#[test]
fn no_tool_callbacks_before_whole_terminal_and_billing_validation() {
    for wire in [
        WorkModelWire::ChatCompletions,
        WorkModelWire::OpenAiResponses,
    ] {
        let stream = if wire == WorkModelWire::ChatCompletions {
            format!(
                "{}{}data: [DONE]\n\n",
                frame(choice(
                    json!({"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":"read_file","arguments":"{}"}}]}),
                    Some("tool_calls")
                )),
                frame(json!({"choices":[],"usage":usage(wire)}))
            )
        } else {
            frame(completed(
                json!([{"type":"function_call","call_id":"call1","name":"read_file","arguments":"{}"}]),
                usage(wire),
            ))
        };
        let events = Mutex::new(Vec::new());
        let mut decoder = GoStream::new(&request(wire), &session(), SCOPE);
        let result = decoder
            .push(stream.as_bytes(), &|event| {
                events.lock().unwrap().push(event)
            })
            .unwrap()
            .unwrap();
        assert_eq!(result.outcome.stop, WorkModelStop::ToolUse);
        assert!(events.lock().unwrap().is_empty());
        assert!(decode(wire, &stream.replace("read_file", "unknown_tool")).is_err());
    }
    let bad = completed(
        json!([{"type":"function_call","call_id":"call1","name":"read_file","arguments":"[]"}]),
        usage(WorkModelWire::OpenAiResponses),
    );
    assert!(decode(WorkModelWire::OpenAiResponses, &frame(bad)).is_err());
}
#[test]
fn billing_sources_are_closed_and_allowance_is_separate_from_cost() {
    for (name, expected) in [
        ("free", BillingSource::Free),
        ("balance", BillingSource::Balance),
        ("go", BillingSource::Go),
        ("go-plus", BillingSource::GoPlus),
        ("byok", BillingSource::Byok),
    ] {
        let mut value = usage(WorkModelWire::ChatCompletions);
        value["opencode"]["billing_source"] = json!(name);
        let (tokens, bill) = parse_usage(&value, WorkModelWire::ChatCompletions).unwrap();
        assert_eq!(bill.source, expected);
        assert_eq!(tokens.cost_micros, None);
    }
    for bad in [json!(-1), json!("0.1"), json!(null), json!(1e20)] {
        let mut value = usage(WorkModelWire::ChatCompletions);
        value["cost"] = bad;
        assert!(parse_usage(&value, WorkModelWire::ChatCompletions).is_err());
    }
    let mut value = usage(WorkModelWire::ChatCompletions);
    value["opencode"]["estimated"] = json!(true);
    assert!(parse_usage(&value, WorkModelWire::ChatCompletions).is_err());
    value["opencode"]["billing_source"] = json!("byok");
    assert!(
        parse_usage(&value, WorkModelWire::ChatCompletions)
            .unwrap()
            .1
            .estimated
    );
}
#[test]
fn billed_breakdown_stays_separate_from_raw_wire_usage() {
    // Exact relationship in the official Console example: wire prompt input
    // need not equal uncached billed input plus billed cache reads.
    let value = json!({"prompt_tokens":1200,"completion_tokens":300,"cost":0.00412,
        "opencode":{"billing_source":"balance","list_cost":0.00412,
        "tokens":{"input":1200,"output":300,"reasoning":120,"cache_read":8000,"cache_write_5m":0,"cache_write_1h":0}}});
    let (raw, bill) = parse_usage(&value, WorkModelWire::ChatCompletions).unwrap();
    assert_eq!(
        (
            raw.input_tokens,
            raw.cached_input_tokens,
            raw.reasoning_tokens
        ),
        (1200, 0, 0)
    );
    assert_eq!(bill.tokens.unwrap().cache_read, 8000);
    let mut bad = value.clone();
    bad["opencode"]["tokens"]["reasoning"] = json!(301);
    assert!(parse_usage(&bad, WorkModelWire::ChatCompletions).is_err());
    bad = value.clone();
    bad["opencode"]["tokens"]["input"] = json!(u64::MAX);
    assert!(parse_usage(&bad, WorkModelWire::ChatCompletions).is_err());
    bad = value.clone();
    bad["total_tokens"] = json!(1499);
    assert!(parse_usage(&bad, WorkModelWire::ChatCompletions).is_err());
}
#[test]
fn replay_is_bound_to_connection_conversation_model_and_protocol() {
    let target = LeadTarget::compatible("https://opencode.ai/inference/go/openai/v1").unwrap();
    for wire in [
        WorkModelWire::ChatCompletions,
        WorkModelWire::OpenAiResponses,
    ] {
        let mut req = request(wire);
        let raw = if wire == WorkModelWire::ChatCompletions {
            json!("reasoning")
        } else {
            json!({"id":"reasoning1","type":"reasoning","summary":[],"encrypted_content":"fixture"})
        };
        let envelope = replay_envelope(wire, &req.model.model, SCOPE, &session(), raw);
        req.messages.push(WorkModelMessage::Assistant(vec![
            WorkModelPart::Replay(envelope.clone()),
            WorkModelPart::Text("Done".into()),
        ]));
        assert!(request_body(&req, &target, &session(), SCOPE).is_ok());
        for key in ["wire", "protocol", "connection_scope", "session", "model"] {
            let mut changed = envelope.clone();
            changed[key] = json!("wrong");
            let mut altered = req.clone();
            altered.messages[1] = WorkModelMessage::Assistant(vec![
                WorkModelPart::Replay(changed),
                WorkModelPart::Text("Done".into()),
            ]);
            assert!(request_body(&altered, &target, &session(), SCOPE).is_err());
        }
        assert!(request_body(
            &req,
            &target,
            &SessionId::parse("c".repeat(64)).unwrap(),
            SCOPE
        )
        .is_err());
    }
}
#[test]
fn unsupported_modalities_and_bounds_fail_before_dispatch() {
    let target = LeadTarget::compatible("https://opencode.ai/inference/go/openai/v1").unwrap();
    let mut req = request(WorkModelWire::ChatCompletions);
    req.native_search = true;
    assert!(request_body(&req, &target, &session(), SCOPE).is_err());
    req.native_search = false;
    req.messages = vec![WorkModelMessage::User(vec![WorkModelPart::Image {
        media_type: "image/png".into(),
        bytes: vec![],
    }])];
    assert!(request_body(&req, &target, &session(), SCOPE).is_err());
    req.messages = vec![WorkModelMessage::User(vec![WorkModelPart::Text(
        "x".repeat(MAX_REQUEST_BYTES + 1),
    )])];
    assert_eq!(
        request_body(&req, &target, &session(), SCOPE).err(),
        Some(GoFault::TooLarge)
    );
    let mut decoder = GoStream::new(&request(WorkModelWire::ChatCompletions), &session(), SCOPE);
    assert_eq!(
        decoder
            .push(&vec![b' '; MAX_STREAM_BYTES + 1], &|_| {})
            .err()
            .unwrap()
            .fault,
        GoFault::TooLarge
    );
}
#[test]
fn source_errors_never_become_generic_retriable_or_claim_uncharged_dispatch() {
    for fault in [
        GoFault::Unauthorized,
        GoFault::Forbidden,
        GoFault::RateLimited {
            retry_after_ms: Some(10),
        },
        GoFault::Protocol,
        GoFault::Incomplete,
        GoFault::UsageUnavailable,
    ] {
        assert!(matches!(
            GoFailure::sent(fault).into_model_error(),
            WorkModelError::OpenCodeGo {
                usage_unknown: true,
                ..
            }
        ));
        assert!(matches!(
            GoFailure::before(fault).into_model_error(),
            WorkModelError::OpenCodeGo {
                usage_unknown: false,
                ..
            }
        ));
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn fixture(
    status: u16,
    content_type: &str,
    body: String,
    extra: &str,
) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let response=format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len());
    let thread = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = vec![];
        let mut buf = [0; 4096];
        loop {
            let n = socket.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                let len = header
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|n| n.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + len {
                    break;
                }
            }
        }
        socket.write_all(response.as_bytes()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (origin, thread)
}
#[test]
fn public_catalog_omits_key_and_inference_sends_own_identity_and_session() {
    runtime().block_on(async {
        let (origin, server) = fixture(
            200,
            "application/json",
            r#"{"data":[{"id":"kimi-k3"}]}"#.into(),
            "",
        );
        let mut c = client();
        c.fixture_origin = Some(origin);
        assert_eq!(c.models().await.unwrap().supported.len(), 1);
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /v1/models "));
        assert!(!request.to_lowercase().contains("authorization"));
        assert!(!request.contains("fixture-go-key"));
        assert!(!request.contains("x-opencode-session"));
        for wire in [
            WorkModelWire::ChatCompletions,
            WorkModelWire::OpenAiResponses,
        ] {
            let body = if wire == WorkModelWire::ChatCompletions {
                chat_text()
            } else {
                frame(response_text())
            };
            let (origin, server) = fixture(200, "text/event-stream", body, "");
            c.fixture_origin = Some(origin);
            assert!(c
                .call(&session(), super::tests::request(wire), &|_| {})
                .await
                .is_ok());
            let req = server.join().unwrap();
            assert!(req.contains("authorization: Bearer fixture-go-key"));
            assert!(req.contains(&format!("x-opencode-session: {}", session().as_str())));
            assert!(req.contains(&format!("user-agent: {USER_AGENT}")));
            assert!(!req.contains("User-Agent: opencode"));
        }
    });
}
#[test]
fn redirects_and_http_failures_never_follow_or_retry() {
    runtime().block_on(async {
        for (status, expected) in [
            (302, GoFault::Protocol),
            (401, GoFault::Unauthorized),
            (403, GoFault::Forbidden),
            (
                429,
                GoFault::RateLimited {
                    retry_after_ms: Some(3000),
                },
            ),
        ] {
            let trap = TcpListener::bind("127.0.0.1:0").unwrap();
            trap.set_nonblocking(true).unwrap();
            let extra = format!(
                "Location: http://{}/steal\r\nRetry-After: 3\r\n",
                trap.local_addr().unwrap()
            );
            let (origin, server) = fixture(
                status,
                "application/json",
                r#"{"error":{"message":"PRIVATE DIAGNOSTIC"}}"#.into(),
                &extra,
            );
            let mut c = client();
            c.fixture_origin = Some(origin);
            let error = c
                .call(&session(), request(WorkModelWire::ChatCompletions), &|_| {})
                .await
                .err()
                .unwrap();
            assert_eq!(error.fault, expected);
            assert!(!format!("{error:?}").contains("PRIVATE"));
            server.join().unwrap();
            assert!(trap.accept().is_err());
        }
    });
}
#[test]
fn numeric_billing_does_not_silently_underflow_or_invent_precision() {
    for raw in [
        r#"{"cost":1e-999}"#,
        r#"{"cost":0.1234567890123456789}"#,
        r#"{"cost":1e309}"#,
    ] {
        assert!(parse_frame(raw).is_err());
    }
    assert!(parse_frame(
        r#"{"cost":0.00412,"tokens":18446744073709551615,"text":"amount 1e-999 is user text"}"#
    )
    .is_ok());
}
#[test]
fn streaming_fragmentation_utf8_and_truncation_preserve_terminal_rules() {
    for wire in [
        WorkModelWire::ChatCompletions,
        WorkModelWire::OpenAiResponses,
    ] {
        let stream = if wire == WorkModelWire::ChatCompletions {
            chat_text()
        } else {
            frame(response_text())
        };
        for size in [1, 2, 3, 7, 64] {
            let mut decoder = GoStream::new(&request(wire), &session(), SCOPE);
            let mut terminal = None;
            for bytes in stream.as_bytes().chunks(size) {
                if let Some(result) = decoder.push(bytes, &|_| {}).unwrap() {
                    assert!(terminal.is_none());
                    terminal = Some(result);
                }
            }
            assert!(terminal.is_some());
        }
        assert!(decode(wire, stream.trim_end_matches('\n')).is_err());
        assert!(decode(wire, &format!("{stream}data: {{}}\n\n")).is_err());
        assert!(decode(wire, &stream.replace("fixture-response", "bad\nresponse")).is_err());
    }
}
#[test]
fn terminal_tool_validation_rejects_duplicate_or_cross_protocol_items() {
    let tool =
        json!({"type":"function_call","call_id":"call1","name":"read_file","arguments":"{}"});
    for output in [
        json!([tool.clone(), tool]),
        json!([{"type":"web_search_call","id":"native-search"}]),
        json!([{"type":"function_call","call_id":"call1","name":"read_file","arguments":"{}","namespace":"other"}]),
    ] {
        assert!(decode(
            WorkModelWire::OpenAiResponses,
            &frame(completed(output, usage(WorkModelWire::OpenAiResponses)))
        )
        .is_err());
    }
    let stream = format!(
        "{}{}{}",
        frame(
            json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"x\":1}"})
        ),
        frame(completed(
            json!([{"type":"function_call","call_id":"call1","name":"read_file","arguments":"{}"}]),
            usage(WorkModelWire::OpenAiResponses)
        )),
        ""
    );
    assert!(decode(WorkModelWire::OpenAiResponses, &stream).is_err());
}
#[test]
fn complete_reply_without_streamed_text_emits_only_validated_text_not_tools() {
    let events = Mutex::new(Vec::new());
    let mut decoder = GoStream::new(&request(WorkModelWire::OpenAiResponses), &session(), SCOPE);
    decoder
        .push(frame(response_text()).as_bytes(), &|event| {
            events.lock().unwrap().push(event)
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e,WorkModelEvent::Text(t) if t=="Hello"))
            .count(),
        1
    );
    let mut bad = response_text();
    bad["response"]["usage"]["opencode"]["billing_source"] = json!("unknown");
    let events = Mutex::new(Vec::new());
    let mut decoder = GoStream::new(&request(WorkModelWire::OpenAiResponses), &session(), SCOPE);
    assert!(decoder
        .push(frame(bad).as_bytes(), &|e| events.lock().unwrap().push(e))
        .is_err());
    assert!(events.lock().unwrap().is_empty());
}
#[test]
fn canceled_http_waiter_drops_callbacks_without_automatic_redispatch() {
    runtime().block_on(async {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();let origin=format!("http://{}",listener.local_addr().unwrap());
        let (tx,rx)=std::sync::mpsc::channel();let (finish_tx,finish_rx)=std::sync::mpsc::channel();
        let server=std::thread::spawn(move||{
            let (mut socket,_)=listener.accept().unwrap();socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut bytes=[0;4096];let n=socket.read(&mut bytes).unwrap();assert!(n>0);tx.send(()).unwrap();
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let body=chat_text();let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            // Dropping a local future is NOT a guarantee of upstream rollback.
            // A remote server may finish and charge. No callback or retry survives.
            let _=socket.write_all(response.as_bytes());
            listener.set_nonblocking(true).unwrap();assert!(listener.accept().is_err());
        });
        let mut c=client();c.fixture_origin=Some(origin);let s=session();let events=Mutex::new(Vec::new());
        let sink=|event|events.lock().unwrap().push(event);
        let mut call=Box::pin(c.call(&s,request(WorkModelWire::ChatCompletions),&sink));
        tokio::select!{ result=&mut call=>panic!("unexpected completion: {result:?}"),_ = async{loop{if rx.try_recv().is_ok(){break}tokio::time::sleep(Duration::from_millis(1)).await;}}=>{} }
        drop(call);finish_tx.send(()).unwrap();
        tokio::task::spawn_blocking(move||server.join().unwrap()).await.unwrap();
        assert!(events.lock().unwrap().is_empty());
    });
}
#[test]
fn unsupported_request_does_not_dispatch_or_fetch_catalog_automatically() {
    runtime().block_on(async {
        let trap = TcpListener::bind("127.0.0.1:0").unwrap();
        trap.set_nonblocking(true).unwrap();
        let mut c = client();
        c.fixture_origin = Some(format!("http://{}", trap.local_addr().unwrap()));
        let mut req = request(WorkModelWire::ChatCompletions);
        req.model.model = "qwen3.8-max".into();
        let error = c.call(&session(), req, &|_| {}).await.err().unwrap();
        assert_eq!(error.fault, GoFault::InvalidRequest);
        assert!(!error.dispatched);
        assert!(trap.accept().is_err());
    });
}
