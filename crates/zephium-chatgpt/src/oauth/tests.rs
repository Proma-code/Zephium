use super::*;
use serde_json::json;

#[test]
fn revocation_discovery_cannot_redirect_tokens_to_another_origin() {
    let valid = json!({"issuer": ISSUER, "revocation_endpoint": REVOKE, "jwks_uri": JWKS});
    assert_eq!(validate_discovery(valid.to_string().as_bytes()), Ok(()));
    for endpoint in [
        "http://auth.openai.com/revoke",
        "https://auth.openai.com.evil.example/revoke",
        "https://example.invalid/revoke",
    ] {
        let mut document = valid.clone();
        document["revocation_endpoint"] = json!(endpoint);
        assert_eq!(
            validate_discovery(document.to_string().as_bytes()),
            Err(AuthError::Protocol)
        );
    }
    for field in ["issuer", "jwks_uri"] {
        let mut document = valid.clone();
        document[field] = json!("https://example.invalid");
        assert_eq!(
            validate_discovery(document.to_string().as_bytes()),
            Err(AuthError::Protocol)
        );
    }
    assert_eq!(validate_discovery(b"{}"), Err(AuthError::Protocol));
}

#[test]
fn revocation_only_client_has_no_token_exchange_refresh_or_inference_endpoint() {
    let source = include_str!("../oauth.rs");
    assert!(!source.contains("authorization_code"));
    assert!(!source.contains("grant_type"));
    assert!(!source.contains("api.openai.com/v1"));
    assert!(source.contains("redirect(reqwest::redirect::Policy::none())"));
    assert!(source.contains("retry(reqwest::retry::never())"));
    assert!(source.contains("MAX_RESPONSE"));
}
