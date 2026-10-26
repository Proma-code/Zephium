use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;
const SCOPE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn request() -> WorkModelRequest {
    WorkModelRequest {
        image_fence: None,
        model: WorkModelRef {
            provider: WorkModelProvider::Compatible,
            wire: WorkModelWire::ChatCompletions,
            model: "fixture/model:small".into(),
        },
        system: vec![WorkModelSystemBlock {
            text: "Use only offered tools.".into(),
            cache: false,
        }],
        tools: vec![WorkModelTool {
            name: "read_page".into(),
            description: "Read an approved page.".into(),
            schema: json!({"type":"object","properties":{},"additionalProperties":false}),
            strict: false,
        }],
        messages: vec![WorkModelMessage::User(vec![WorkModelPart::Text(
            "Hello".into(),
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

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}
fn choice(delta: Value, finish: Option<&str>) -> Value {
    json!({"object":"chat.completion.chunk","id":"fixture-reply","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
}
fn complete_text() -> String {
    format!(
        "{}{}{}data: [DONE]\n\n",
        frame(choice(json!({"content":"Hello"}), None)),
        frame(choice(json!({}), Some("stop"))),
        frame(
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}})
        )
    )
}
fn decode(stream: &str) -> Result<GatewayOutcome, GatewayFailure> {
    let mut decoder = GatewayStream::new(&request(), "http://127.0.0.1:8045/v1", SCOPE);
    decoder
        .push(stream.as_bytes(), &|_| {})?
        .ok_or_else(|| decoder.failure(GatewayFault::Incomplete))
}

#[test]
fn endpoint_is_exact_canonical_loopback_with_explicit_port() {
    for (input, expected) in [
        ("http://127.0.0.1:8045/v1", "http://127.0.0.1:8045/v1"),
        ("http://127.0.0.1:80/v1/", "http://127.0.0.1:80/v1"),
        ("http://[::1]:65535/v1", "http://[::1]:65535/v1"),
    ] {
        assert_eq!(LoopbackBase::parse(input).unwrap().as_str(), expected);
    }
    for input in [
        "http://localhost:8045/v1",
        "https://127.0.0.1:8045/v1",
        "http://127.0.0.1/v1",
        "http://127.0.0.1:0/v1",
        "http://127.0.0.1:65536/v1",
        "http://127.0.0.1:080/v1",
        "http://0.0.0.0:8045/v1",
        "http://127.0.0.2:8045/v1",
        "http://2130706433:8045/v1",
        "http://0177.0.0.1:8045/v1",
        "http://127.1:8045/v1",
        "http://0x7f000001:8045/v1",
        "http://[::ffff:127.0.0.1]:8045/v1",
        "http://[0:0:0:0:0:0:0:1]:8045/v1",
        "http://127.0.0.1.example:8045/v1",
        "http://user:pass@127.0.0.1:8045/v1",
        "http://127.0.0.1:8045/v1?key=bad",
        "http://127.0.0.1:8045/v1#bad",
        "http://127.0.0.1:8045/a/../v1",
        "http://127.0.0.1:8045/%76%31",
        "http://127.0.0.1:8045/v1//",
        " http://127.0.0.1:8045/v1",
        "http://127.0.0.1:8045/v1\n",
        "HTTP://127.0.0.1:8045/v1",
    ] {
        assert_eq!(
            LoopbackBase::parse(input).err(),
            Some(GatewayFault::InvalidEndpoint),
            "{input}"
        );
    }
}

#[test]
fn catalog_is_bounded_and_never_claims_entitlement() {
    let rows = parse_models(br#"{"object":"list","data":[{"id":"fixture/model:small"},{"id":"second","display_name":"Second"}]}"#).unwrap();
    assert_eq!(rows[0].id, "fixture/model:small");
    assert_eq!(rows[1].display_name, "Second");
    assert!(parse_models(br#"{"data":[]}"#).unwrap().is_empty());
    for input in [
        br#"{}"#.as_slice(),
        br#"{"models":[]}"#.as_slice(),
        br#"{"data":[{"id":"secret\nname"}]}"#.as_slice(),
        br#"{"data":[{"id":"same"},{"id":"same"}]}"#.as_slice(),
    ] {
        assert_eq!(parse_models(input).err(), Some(GatewayFault::Protocol));
    }
    assert_eq!(
        parse_models(&vec![b' '; MAX_CATALOG_BYTES + 1]).err(),
        Some(GatewayFault::TooLarge)
    );
}

#[test]
fn terminal_requires_known_finish_and_done_and_keeps_usage_optional() {
    let result = decode(&complete_text()).unwrap();
    assert_eq!(result.stop, WorkModelStop::EndTurn);
    assert_eq!(result.usage.unwrap().input_tokens, 10);
    let no_usage = format!(
        "{}data: [DONE]\n\n",
        frame(choice(json!({"content":"Hi"}), Some("stop")))
    );
    assert!(decode(&no_usage).unwrap().usage.is_none());
    for stream in [
        frame(choice(json!({"content":"Hi"}), None)),
        frame(choice(json!({}), Some("stop"))),
        "data: [DONE]\n\n".into(),
        format!(
            "{}data: [DONE]\n\n",
            frame(choice(json!({}), Some("unknown")))
        ),
        format!(
            "{}data: [DONE]\n\n",
            frame(choice(json!({"content":"partial"}), Some("length")))
        ),
    ] {
        assert!(decode(&stream).is_err(), "{stream}");
    }
}

#[test]
fn tool_events_wait_for_complete_terminal_and_validated_arguments() {
    let tool = json!({"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":"read_page","arguments":"{}"}}]});
    let prefix = format!(
        "{}{}",
        frame(choice(tool, None)),
        frame(choice(json!({}), Some("tool_calls")))
    );
    let events = Mutex::new(Vec::new());
    let sink = |event| events.lock().unwrap().push(event);
    let mut decoder = GatewayStream::new(&request(), "http://127.0.0.1:8045/v1", SCOPE);
    assert!(decoder.push(prefix.as_bytes(), &sink).unwrap().is_none());
    assert!(events.lock().unwrap().is_empty());
    let result = decoder.push(b"data: [DONE]\n\n", &sink).unwrap().unwrap();
    assert_eq!(result.stop, WorkModelStop::ToolUse);
    assert!(
        events.lock().unwrap().is_empty(),
        "only the bridge may commit tools"
    );
    assert!(matches!(
        result.assistant.last(),
        Some(WorkModelPart::ToolCall(_))
    ));
    for (name, args) in [
        ("run_shell", "{}"),
        ("read_page", "[]"),
        ("read_page", "invalid"),
    ] {
        let stream = format!(
            "{}data: [DONE]\n\n",
            frame(choice(
                json!({"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":name,"arguments":args}}]}),
                Some("tool_calls")
            ))
        );
        assert_eq!(decode(&stream).err().unwrap().fault, GatewayFault::Protocol);
    }
}

#[test]
fn partial_failure_is_not_success_retry_or_zero_usage() {
    let mut decoder = GatewayStream::new(&request(), "http://127.0.0.1:8045/v1", SCOPE);
    decoder
        .push(
            frame(choice(json!({"content":"partial"}), None)).as_bytes(),
            &|_| {},
        )
        .unwrap();
    let failure = decoder
        .push(
            frame(json!({"error":{"code":"rate_limit_exceeded","message":"untrusted secret"}}))
                .as_bytes(),
            &|_| {},
        )
        .err()
        .unwrap();
    assert_eq!(
        failure.fault,
        GatewayFault::RateLimited {
            retry_after_ms: None
        }
    );
    assert!(failure.dispatched && failure.output_started);
    assert!(!format!("{failure:?}").contains("secret"));
}

/// One fixed response, capturing only synthetic request fixtures.
fn server(
    status: u16,
    content_type: &str,
    extra_headers: &str,
    body: &str,
) -> (LoopbackBase, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let reply = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",body.len());
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (mut socket, _) = loop {
            match listener.accept() {
                Ok(socket) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "fixture was not contacted"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = socket.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..at]).to_ascii_lowercase();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= at + 4 + length {
                    break;
                }
            }
            assert!(bytes.len() < MAX_REQUEST_BYTES + 8192);
        }
        socket.write_all(reply.as_bytes()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (
        LoopbackBase::parse(&format!("http://127.0.0.1:{port}/v1")).unwrap(),
        handle,
    )
}
fn client(base: LoopbackBase) -> GatewayClient {
    GatewayClient::new(
        base,
        Arc::new(LeadSecret::new("synthetic-gateway-key".into()).unwrap()),
        SCOPE.into(),
    )
    .unwrap()
}

#[tokio::test]
async fn fake_http_uses_exact_paths_gateway_bearer_and_completed_stream() {
    let (base, captured) = server(
        200,
        "application/json",
        "",
        r#"{"data":[{"id":"fixture/model:small"}]}"#,
    );
    assert_eq!(client(base).models().await.unwrap().len(), 1);
    let request = captured.join().unwrap();
    assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer synthetic-gateway-key\r\n"));
    let (base, captured) = server(
        200,
        "text/event-stream; charset=utf-8",
        "",
        &complete_text(),
    );
    let output = client(base).call(request_fixture(), &|_| {}).await.unwrap();
    assert_eq!(output.stop, WorkModelStop::EndTurn);
    let bytes = captured.join().unwrap();
    assert!(bytes.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    let body: Value = serde_json::from_str(bytes.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "fixture/model:small");
}
fn request_fixture() -> WorkModelRequest {
    request()
}

#[tokio::test]
async fn redirects_are_not_followed_and_rate_limits_are_not_retried() {
    let redirected = TcpListener::bind("127.0.0.1:0").unwrap();
    redirected.set_nonblocking(true).unwrap();
    let location = format!(
        "Location: http://127.0.0.1:{}/stolen\r\n",
        redirected.local_addr().unwrap().port()
    );
    let (base, captured) = server(302, "application/json", &location, "{}");
    assert_eq!(
        client(base).models().await.unwrap_err().fault,
        GatewayFault::Protocol
    );
    captured.join().unwrap();
    assert_eq!(
        redirected.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let (base, captured) = server(
        429,
        "application/json",
        "Retry-After: 7\r\n",
        r#"{"error":{"message":"secret"}}"#,
    );
    assert_eq!(
        client(base)
            .call(request(), &|_| {})
            .await
            .unwrap_err()
            .fault,
        GatewayFault::RateLimited {
            retry_after_ms: Some(7000)
        }
    );
    captured.join().unwrap();
}

#[test]
fn request_validation_never_drops_unsupported_parts_or_cross_source_replay() {
    let origin = "http://127.0.0.1:8045/v1";
    let target = LeadTarget::compatible(origin).unwrap();
    let replay = json!({"wire":"local_gateway","origin":origin,"connection_scope":SCOPE,"model":"fixture/model:small","reasoning_content":"synthetic reasoning"});
    let mut req = request();
    req.messages = vec![WorkModelMessage::User(vec![WorkModelPart::Replay(
        replay.clone(),
    )])];
    assert_eq!(
        request_body(&req, &target, origin, SCOPE).unwrap_err(),
        GatewayFault::InvalidRequest
    );
    req.messages = vec![WorkModelMessage::Assistant(vec![WorkModelPart::Replay(
        replay.clone(),
    )])];
    assert_eq!(
        request_body(&req, &target, origin, SCOPE).unwrap_err(),
        GatewayFault::InvalidRequest
    );
    req.messages = vec![WorkModelMessage::Assistant(vec![
        WorkModelPart::Replay(replay),
        WorkModelPart::Text("Hello".into()),
    ])];
    let mut injected = req.clone();
    if let WorkModelMessage::Assistant(parts) = &mut injected.messages[0] {
        if let WorkModelPart::Replay(value) = &mut parts[0] {
            value["injected"] = json!({"messages":"must never be forwarded or cloned"});
        }
    }
    assert_eq!(
        request_body(&injected, &target, origin, SCOPE).unwrap_err(),
        GatewayFault::InvalidRequest
    );
    let body: Value =
        serde_json::from_slice(&request_body(&req, &target, origin, SCOPE).unwrap()).unwrap();
    assert_eq!(
        body["messages"][1]["reasoning_content"],
        "synthetic reasoning"
    );
    assert!(body["messages"][1].get("injected").is_none());
    assert_eq!(
        request_body(&req, &target, "http://127.0.0.1:9000/v1", SCOPE).unwrap_err(),
        GatewayFault::InvalidRequest
    );
    assert_eq!(
        request_body(&req, &target, origin, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap_err(),
        GatewayFault::InvalidRequest
    );
    req.model.model = "other-model".into();
    assert_eq!(
        request_body(&req, &target, origin, SCOPE).unwrap_err(),
        GatewayFault::InvalidRequest
    );
    req = request();
    req.cache_key = Some("x".repeat(257));
    assert_eq!(
        request_body(&req, &target, origin, SCOPE).unwrap_err(),
        GatewayFault::InvalidRequest
    );
    req = request();
    req.tool_choice = WorkModelToolChoice::Named("not_offered".into());
    assert_eq!(
        request_body(&req, &target, origin, SCOPE).unwrap_err(),
        GatewayFault::InvalidRequest
    );
    req = request();
    req.messages = vec![WorkModelMessage::User(vec![WorkModelPart::Text(
        "x".repeat(MAX_REQUEST_BYTES + 1),
    )])];
    assert_eq!(
        request_body(&req, &target, origin, SCOPE).unwrap_err(),
        GatewayFault::TooLarge
    );
}

#[test]
fn byte_fragmentation_unicode_crlf_and_comments_preserve_terminal() {
    let stream = format!(
        ": heartbeat\n\n{}data: [DONE]\n\n",
        frame(choice(json!({"content":"مرحباً 🌍"}), Some("stop")))
    )
    .replace('\n', "\r\n");
    let mut decoder = GatewayStream::new(&request(), "http://127.0.0.1:8045/v1", SCOPE);
    let events = Mutex::new(String::new());
    let sink = |event| {
        if let WorkModelEvent::Text(text) = event {
            events.lock().unwrap().push_str(&text);
        }
    };
    let mut outcome = None;
    for byte in stream.bytes() {
        if let Some(result) = decoder.push(&[byte], &sink).unwrap() {
            outcome = Some(result);
        }
    }
    assert_eq!(outcome.unwrap().stop, WorkModelStop::EndTurn);
    assert_eq!(*events.lock().unwrap(), "مرحباً 🌍");
}

#[test]
fn malformed_usage_and_tool_terminals_fail_closed() {
    for usage in [
        json!({"prompt_tokens":10}),
        json!({"prompt_tokens":-1,"completion_tokens":2}),
        json!({"prompt_tokens":10,"completion_tokens":2,"total_tokens":13}),
        json!({"prompt_tokens":u64::MAX,"completion_tokens":1,"total_tokens":null}),
        json!({"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":42}),
        json!({"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":11}}),
        json!({"prompt_tokens":10,"completion_tokens":2,"completion_tokens_details":{"reasoning_tokens":3}}),
    ] {
        assert_eq!(parse_usage(&usage).err(), Some(GatewayFault::Protocol));
    }
    let usage =
        parse_usage(&json!({"prompt_tokens":10,"completion_tokens":2,"cost_micros":0,"cost":0}))
            .unwrap();
    assert_eq!(
        usage.cost_micros, None,
        "gateway dollar amounts are not authoritative"
    );
    for calls in [
        json!([{"index":0,"function":{"name":"read_page","arguments":"{}"}}]),
        json!([{"index":1,"id":"call1","function":{"name":"read_page","arguments":"{}"}}]),
        json!([{"index":0,"id":"same","function":{"name":"read_page","arguments":"{}"}},{"index":1,"id":"same","function":{"name":"read_page","arguments":"{}"}}]),
    ] {
        let stream = format!(
            "{}data: [DONE]\n\n",
            frame(choice(json!({"tool_calls":calls}), Some("tool_calls")))
        );
        assert_eq!(decode(&stream).unwrap_err().fault, GatewayFault::Protocol);
    }
    let mut stream = complete_text();
    stream.push_str(&frame(choice(json!({"content":"after terminal"}), None)));
    assert_eq!(decode(&stream).unwrap_err().fault, GatewayFault::Protocol);
}

#[test]
fn required_usage_and_failure_mapping_never_claim_free_or_retriable_work() {
    let stream = format!(
        "{}data: [DONE]\n\n",
        frame(choice(json!({"content":"Hello"}), Some("stop")))
    );
    let failure = decode(&stream).unwrap().require_usage().err().unwrap();
    assert_eq!(failure.fault, GatewayFault::UsageUnavailable);
    assert_eq!(
        failure.into_model_error(),
        WorkModelError::Gateway {
            fault: GatewayModelFault::UsageUnavailable,
            usage_unknown: true
        }
    );
    assert_eq!(
        decode(&complete_text())
            .unwrap()
            .require_usage()
            .unwrap()
            .usage
            .cost_micros,
        None
    );
    for fault in [
        GatewayFault::Unauthorized,
        GatewayFault::Forbidden,
        GatewayFault::RateLimited {
            retry_after_ms: Some(1000),
        },
        GatewayFault::Timeout,
        GatewayFault::Protocol,
        GatewayFault::Incomplete,
    ] {
        assert!(matches!(
            GatewayFailure::sent(fault).into_model_error(),
            WorkModelError::Gateway {
                usage_unknown: true,
                ..
            }
        ));
        assert!(matches!(
            GatewayFailure::before(fault).into_model_error(),
            WorkModelError::Gateway {
                usage_unknown: false,
                ..
            }
        ));
    }
}

#[tokio::test]
async fn fake_http_eof_media_and_overlarge_catalog_never_become_success() {
    for (media, body, expected) in [
        ("application/json", complete_text(), GatewayFault::Protocol),
        (
            "text/event-stream",
            frame(choice(json!({"content":"partial"}), None)),
            GatewayFault::Incomplete,
        ),
        (
            "text/event-stream",
            format!(
                "{}data: [DONE]",
                frame(choice(json!({"content":"Hi"}), Some("stop")))
            ),
            GatewayFault::Incomplete,
        ),
    ] {
        let (base, captured) = server(200, media, "", &body);
        let failure = client(base).call(request(), &|_| {}).await.unwrap_err();
        assert_eq!(failure.fault, expected);
        assert!(failure.dispatched);
        captured.join().unwrap();
    }
    let (base, captured) = server(
        200,
        "application/json",
        "",
        &" ".repeat(MAX_CATALOG_BYTES + 1),
    );
    let failure = client(base).models().await.unwrap_err();
    assert_eq!(failure.fault, GatewayFault::TooLarge);
    assert!(!failure.dispatched);
    // Early bounded-body rejection can reset the synthetic socket while writing.
    let _ = captured.join();
}

#[tokio::test]
async fn dropping_inflight_call_closes_its_stream_without_retry() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = LoopbackBase::parse(&format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    ))
    .unwrap();
    let (started, receive) = tokio::sync::oneshot::channel();
    let observed = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() < MAX_REQUEST_BYTES + 8192);
            if let Some(at) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..at]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= at + 4 + length {
                    break;
                }
            }
        }
        let body = frame(choice(json!({"content":"partial"}), None));
        let response=format!("HTTP/1.1 200 Fixture\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}");
        socket.write_all(response.as_bytes()).await.unwrap();
        started.send(()).unwrap();
        // No wire terminal is sent. Dropping the client must release the stream.
        let closed = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut buffer))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0)) || closed.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(80), listener.accept())
                .await
                .is_err(),
            "no replacement request after cancellation"
        );
    });
    let task = tokio::spawn(async move { client(base).call(request(), &|_| {}).await });
    tokio::time::timeout(Duration::from_secs(3), receive)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    observed.await.unwrap();
}
