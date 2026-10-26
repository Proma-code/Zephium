use super::*;
use crate::provider_transport::lead::LeadSecret;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn client() -> OpenCodeGoClient {
    OpenCodeGoClient::new(
        Arc::new(LeadSecret::new("fixture-quota-key".into()).unwrap()),
        "a".repeat(32),
    )
    .unwrap()
}

fn window(percent: f64, status: &str) -> Value {
    json!({"status":status,"percent":percent,"resetsAt":"2026-10-26T12:34:56.789Z"})
}

fn body() -> Value {
    json!({"usage": {
        "rolling":window(12.5, "ok"),
        "weekly":window(99.5, "rate-limited"),
        "monthly":window(100.0, "ok"),
    }})
}

fn parse(value: &Value) -> Result<GoQuotaSnapshot, GoQuotaFault> {
    parse_snapshot(&serde_json::to_vec(value).unwrap())
}

fn read_request(socket: &mut TcpStream) -> String {
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = vec![];
    loop {
        let mut buffer = [0; 1024];
        let read = socket.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        assert!(bytes.len() <= 8192);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(bytes).unwrap();
        }
    }
}

struct Fixture {
    origin: String,
    server: std::thread::JoinHandle<(String, TcpListener)>,
}

impl Fixture {
    fn raw(response: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = read_request(&mut socket);
            // Oversize rejection may deliberately close before the fixture
            // finishes writing. No real account/key or external host is used.
            let _ = socket.write_all(&response);
            (request, listener)
        });
        Self { origin, server }
    }

    fn response(status: u16, media: &str, body: &str, extra: &str) -> Self {
        Self::raw(format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        ).into_bytes())
    }

    fn client(&self) -> OpenCodeGoClient {
        let mut client = client();
        client.fixture_origin = Some(self.origin.clone());
        client
    }

    fn finish(self) -> String {
        let (request, listener) = self.server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        request
    }
}

#[test]
fn quota_preserves_exact_percent_and_independent_limited_status() {
    let value = parse(&body()).unwrap();
    assert_eq!(value.rolling.used_percent, 12.5);
    assert_eq!(value.rolling.remaining_percent, 87.5);
    assert_eq!(value.rolling.resets_at_ms, 1_793_018_096_789);
    assert!(!value.rolling.limited);
    assert_eq!(value.weekly.remaining_percent, 0.5);
    assert!(value.weekly.limited);
    // A rounded 100% is NOT proof that Go is refusing requests.
    assert_eq!(value.monthly.remaining_percent, 0.0);
    assert!(!value.monthly.limited);
    for (used, remaining) in [(0.0, 100.0), (25.125, 74.875), (100.0, 0.0)] {
        assert_eq!(
            parse_window(&window(used, "ok")).unwrap().remaining_percent,
            remaining
        );
    }
}

#[test]
fn quota_requires_all_three_complete_windows_atomically() {
    for name in ["rolling", "weekly", "monthly"] {
        let mut value = body();
        value["usage"].as_object_mut().unwrap().remove(name);
        assert_eq!(parse(&value), Err(GoQuotaFault::Protocol));
        for field in ["percent", "status", "resetsAt"] {
            let mut value = body();
            value["usage"][name].as_object_mut().unwrap().remove(field);
            assert_eq!(parse(&value), Err(GoQuotaFault::Protocol));
        }
        for invalid in [Value::Null, json!([]), json!("unknown"), json!({})] {
            let mut value = body();
            value["usage"][name] = invalid;
            assert_eq!(parse(&value), Err(GoQuotaFault::Protocol));
        }
    }
}

#[test]
fn quota_never_coerces_or_clamps_invalid_percentages() {
    for invalid in [
        json!(-0.01),
        json!(100.01),
        json!("0"),
        json!("NaN"),
        json!(null),
        json!(true),
        json!({}),
        json!([]),
    ] {
        let mut value = body();
        value["usage"]["rolling"]["percent"] = invalid;
        assert_eq!(parse(&value), Err(GoQuotaFault::Protocol));
    }
    for raw in [
        "NaN",
        "Infinity",
        "1e999",
        "1e-999",
        "100.0000000000000001",
        "-1e-999",
    ] {
        let source = body().to_string().replace("12.5", raw);
        assert_eq!(
            parse_snapshot(source.as_bytes()),
            Err(GoQuotaFault::Protocol)
        );
    }
}

#[test]
fn quota_rejects_unknown_status_instead_of_inferencing_from_percentage() {
    for invalid in [
        json!("limited"),
        json!("OK"),
        json!("rate_limited"),
        json!(false),
        json!(null),
    ] {
        let mut value = body();
        value["usage"]["weekly"]["status"] = invalid;
        assert_eq!(parse(&value), Err(GoQuotaFault::Protocol));
    }
}

#[test]
fn quota_dates_are_provider_instants_not_plan_or_local_time_estimates() {
    for (date, expected) in [
        ("1970-01-01T00:00:00Z", 0),
        ("1970-01-01T01:00:00+01:00", 0),
        ("1970-01-01T00:00:00.001Z", 1),
        ("1970-01-01T00:00:00.1Z", 100),
        ("1970-01-01T00:00:00.123456789Z", 123),
        ("2000-02-29T00:00:00Z", 951_782_400_000),
        ("2026-10-26T12:34:56.789Z", 1_793_018_096_789),
        ("2026-10-26T13:34:56.789+01:00", 1_793_018_096_789),
        ("9999-12-31T23:59:59.999Z", 253_402_300_799_999),
    ] {
        assert_eq!(parse_instant(date), Some(expected), "{date}");
    }
    for invalid in [
        "",
        "now",
        "1793018096789",
        "2026-10-26",
        "2026-10-26T12:34:56",
        "2026-02-29T00:00:00Z",
        "2100-02-29T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-01-00T00:00:00Z",
        "2026-01-01T24:00:00Z",
        "2026-01-01T00:60:00Z",
        "2026-01-01T00:00:00+24:00",
        "2026-01-01T00:00:00+00:60",
        "1969-12-31T23:59:59.999999999Z",
        "1970-01-01T00:00:00+01:00",
        "2026-01-01T00:00:00Z trailing",
        "+10000-01-01T00:00:00Z",
    ] {
        assert_eq!(parse_instant(invalid), None, "{invalid}");
    }
    for invalid in [json!(0), json!(null), json!("2026-02-30T00:00:00Z")] {
        let mut value = body();
        value["usage"]["rolling"]["resetsAt"] = invalid;
        assert_eq!(parse(&value), Err(GoQuotaFault::Protocol));
    }
}

#[test]
fn quota_rejects_malformed_200_and_discards_unrelated_identity_and_billing() {
    for malformed in [
        b"".as_slice(),
        b"{",
        b"null",
        b"[]",
        b"{}",
        br#"{"usage":{}}"#,
        br#"{"usage":[]}"#,
        br#"{"usage":null}"#,
        &[0xff],
    ] {
        assert_eq!(parse_snapshot(malformed), Err(GoQuotaFault::Protocol));
    }
    let mut value = body();
    value["email"] = json!("fixture-private@example.invalid");
    value["accountId"] = json!("fixture-private-account");
    value["plan"] = json!("fixture-private-plan");
    value["balance"] = json!(100000);
    value["usage"]["other"] = window(99.0, "rate-limited");
    let snapshot = parse(&value).unwrap();
    assert_eq!(snapshot, parse(&body()).unwrap());
    assert!(!format!("{snapshot:?}").contains("fixture-private"));
    assert_eq!(
        parse_snapshot(&vec![b' '; MAX_QUOTA_BYTES + 1]),
        Err(GoQuotaFault::TooLarge)
    );
}

#[test]
fn quota_get_uses_only_own_key_and_fixed_usage_path() {
    runtime().block_on(async {
        let fixture = Fixture::response(
            200,
            "application/json; charset=utf-8",
            &body().to_string(),
            "",
        );
        let native = fixture.client();
        assert!(native.quota().await.is_ok());
        let request = fixture.finish().to_lowercase();
        assert!(request.starts_with("get /v1/usage http/1.1\r\n"));
        assert!(request.contains("authorization: bearer fixture-quota-key\r\n"));
        assert!(request.contains("accept: application/json\r\n"));
        assert!(request.contains("accept-encoding: identity\r\n"));
        for unwanted in [
            "x-opencode-org-id",
            "x-org-id",
            "x-opencode-session",
            "cookie:",
            "referer:",
            "content-length:",
        ] {
            assert!(!request.contains(unwanted));
        }
        assert_eq!(
            client().endpoint("v1/usage"),
            "https://opencode.ai/inference/go/v1/usage"
        );
    });
}

#[test]
fn quota_403_requires_exact_reviewed_category_not_message_text() {
    for (body, expected) in [
        (
            r#"{"type":"error","error":{"type":"EntitlementError","message":"PRIVATE"}}"#,
            GoQuotaFault::NoEntitlement,
        ),
        (
            r#"{"type":"error","error":{"type":"AuthError","message":"OpenCode Go subscription required."}}"#,
            GoQuotaFault::Forbidden,
        ),
        (
            r#"{"error":{"type":"EntitlementError"}}"#,
            GoQuotaFault::Forbidden,
        ),
        (
            r#"{"type":"error","error":{"type":"entitlement_error"}}"#,
            GoQuotaFault::Forbidden,
        ),
        (
            r#"{"type":"error","error":{"message":"EntitlementError"}}"#,
            GoQuotaFault::Forbidden,
        ),
        ("malformed PRIVATE", GoQuotaFault::Forbidden),
    ] {
        assert_eq!(forbidden_fault(body.as_bytes()), expected);
        assert!(!format!("{expected:?}").contains("PRIVATE"));
    }
}

#[test]
fn quota_http_errors_are_distinct_without_retries_or_backend_text() {
    runtime().block_on(async {
        for (status, response, expected) in [
            (
                401,
                r#"{"type":"error","error":{"type":"AuthError","message":"PRIVATE"}}"#,
                GoQuotaFault::Unauthorized,
            ),
            (
                403,
                r#"{"type":"error","error":{"type":"EntitlementError","message":"PRIVATE"}}"#,
                GoQuotaFault::NoEntitlement,
            ),
            (
                403,
                r#"{"type":"error","error":{"type":"Forbidden","message":"PRIVATE"}}"#,
                GoQuotaFault::Forbidden,
            ),
            (403, "malformed PRIVATE", GoQuotaFault::Forbidden),
            (404, "PRIVATE", GoQuotaFault::EndpointUnavailable),
            (405, "PRIVATE", GoQuotaFault::EndpointUnavailable),
            (408, "PRIVATE", GoQuotaFault::Timeout),
            (413, "PRIVATE", GoQuotaFault::TooLarge),
            (
                429,
                "PRIVATE",
                GoQuotaFault::RateLimited {
                    retry_after_ms: Some(3000),
                },
            ),
            (500, "PRIVATE", GoQuotaFault::Unavailable),
            (502, "PRIVATE", GoQuotaFault::Unavailable),
            (503, "PRIVATE", GoQuotaFault::Unavailable),
            (504, "PRIVATE", GoQuotaFault::Timeout),
            (200, "malformed PRIVATE", GoQuotaFault::Protocol),
        ] {
            let fixture =
                Fixture::response(status, "application/json", response, "Retry-After: 3\r\n");
            let error = fixture.client().quota().await.unwrap_err();
            assert_eq!(error, expected, "{status}");
            assert!(!format!("{error:?}").contains("PRIVATE"));
            fixture.finish();
        }
    });
}

#[test]
fn quota_retry_after_is_bounded_metadata_not_a_retry_instruction() {
    for (raw, expected) in [
        ("0", Some(0)),
        ("3", Some(3000)),
        ("86400", Some(86_400_000)),
        ("86401", None),
        ("+3", None),
        ("-1", None),
        ("0.5", None),
        ("18446744073709551615", None),
        ("Mon, 26 Oct 2026 00:00:00 GMT", None),
    ] {
        let header = reqwest::header::HeaderValue::from_str(raw).unwrap();
        assert_eq!(
            status_fault(429, Some(&header)),
            GoQuotaFault::RateLimited {
                retry_after_ms: expected
            }
        );
    }
}

#[test]
fn quota_redirects_do_not_leak_key_or_try_a_fallback_endpoint() {
    runtime().block_on(async {
        for status in [301, 302, 303, 307, 308] {
            let trap = TcpListener::bind("127.0.0.1:0").unwrap();
            trap.set_nonblocking(true).unwrap();
            let fixture = Fixture::response(status, "application/json", "{}", &format!("Location: http://{}/steal\r\n", trap.local_addr().unwrap()));
            assert_eq!(fixture.client().quota().await, Err(GoQuotaFault::Protocol));
            fixture.finish();
            assert!(matches!(trap.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        }
    });
}

#[test]
fn quota_wrong_media_never_becomes_a_success_or_entitlement_claim() {
    runtime().block_on(async {
        for (status, expected) in [
            (200, GoQuotaFault::Protocol),
            (403, GoQuotaFault::Forbidden),
        ] {
            let fixture = Fixture::response(status, "text/html", &body().to_string(), "");
            assert_eq!(fixture.client().quota().await, Err(expected));
            fixture.finish();
        }
    });
}

#[test]
fn quota_declared_and_chunked_body_limits_apply_to_success_and_forbidden() {
    runtime().block_on(async {
        for status in [200, 403] {
            for chunked in [false, true] {
                let oversized = "x".repeat(MAX_QUOTA_BYTES + 1);
                let raw = if chunked {
                    format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{oversized}\r\n0\r\n\r\n", oversized.len())
                } else {
                    // Headers alone must reject a declared oversized response.
                    format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", oversized.len())
                };
                let fixture = Fixture::raw(raw.into_bytes());
                assert_eq!(fixture.client().quota().await, Err(GoQuotaFault::TooLarge));
                fixture.finish();
            }
        }
    });
}

#[test]
fn quota_truncated_body_is_transient_not_a_zero_reading() {
    runtime().block_on(async {
        let fixture = Fixture::raw(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{".to_vec());
        assert_eq!(fixture.client().quota().await, Err(GoQuotaFault::Unavailable));
        fixture.finish();
    });
}

#[test]
fn quota_network_refusal_is_transient_without_endpoint_fallback() {
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut native = client();
        native.fixture_origin = Some(format!("http://{}", listener.local_addr().unwrap()));
        drop(listener);
        assert_eq!(native.quota().await, Err(GoQuotaFault::Unavailable));
    });
}

#[test]
fn quota_stalled_body_and_header_are_bounded_without_retries() {
    runtime().block_on(async {
        for send_headers in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut native = client();
            native.fixture_origin = Some(format!("http://{}", listener.local_addr().unwrap()));
            let (finish_tx, finish_rx) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                read_request(&mut socket);
                if send_headers {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{").unwrap();
                }
                finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                listener.set_nonblocking(true).unwrap();
                assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
            });
            assert_eq!(native.quota_with_limits(Duration::from_millis(150), Duration::from_millis(50)).await, Err(GoQuotaFault::Timeout));
            finish_tx.send(()).unwrap();
            server.join().unwrap();
        }
    });
}

#[test]
fn quota_cancellation_drops_local_waiter_without_background_retry_or_result() {
    runtime().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut native = client();
        native.fixture_origin = Some(format!("http://{}", listener.local_addr().unwrap()));
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            read_request(&mut socket);
            seen_tx.send(()).unwrap();
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let body = body().to_string();
            let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        });
        let mut future = Box::pin(native.quota());
        tokio::select! {
            result = &mut future => panic!("unexpected completion: {result:?}"),
            seen = seen_rx => seen.unwrap(),
        }
        drop(future);
        finish_tx.send(()).unwrap();
        server.join().unwrap();
    });
}

#[test]
fn quota_future_dropped_before_polling_makes_no_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut native = client();
    native.fixture_origin = Some(format!("http://{}", listener.local_addr().unwrap()));
    drop(native.quota());
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}
