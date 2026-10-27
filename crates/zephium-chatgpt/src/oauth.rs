//! Bounded fixed-origin revocation of existing legacy ChatGPT registrations.
//! No provider CLI credentials or browser cookies are imported.
use std::time::Duration;

use zeroize::Zeroizing;

use crate::token::Tokens;

const ISSUER: &str = "https://auth.openai.com";
const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const JWKS: &str = "https://auth.openai.com/.well-known/jwks.json";
const REVOKE: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
const MAX_RESPONSE: usize = 128 * 1024;
/// Closed, secret-free errors for native account UX.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthError {
    /// HTTP client configuration was unavailable.
    Configuration,
    /// The request could not reach the fixed trusted service.
    Transport,
    /// The bounded request deadline elapsed.
    Timeout,
    /// A response or protected token record exceeded its byte budget.
    TooLarge,
    /// A protected record or revocation response violated its contract.
    Protocol,
    /// The trusted endpoint did not confirm revocation.
    OAuthRejected,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for AuthError {}

/// Revocation-only HTTP client. Redirects and ambient proxies are off.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    /// Create a bounded HTTPS client without making a request.
    pub fn new() -> Result<Self, AuthError> {
        let http = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20))
            .user_agent("Zephium")
            .build()
            .map_err(|_| AuthError::Configuration)?;
        Ok(Self { http })
    }

    /// Attempt to revoke only this registration's renewable session.
    ///
    /// On failure, the host may clear local credentials, but must report that
    /// remote revocation was not confirmed. This never deletes the registration.
    pub async fn revoke(&self, tokens: &Tokens) -> Result<(), AuthError> {
        let discovery = self.http.get(DISCOVERY).send().await.map_err(http_error)?;
        let body = bounded(discovery).await?;
        validate_discovery(&body)?;
        let response = self
            .http
            .post(REVOKE)
            .form(&[
                ("token", tokens.0.refresh_token.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", tokens.0.identity.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(http_error)?;
        if response.status() == reqwest::StatusCode::OK {
            Ok(())
        } else {
            Err(AuthError::OAuthRejected)
        }
    }
}

fn http_error(error: reqwest::Error) -> AuthError {
    if error.is_timeout() {
        AuthError::Timeout
    } else {
        AuthError::Transport
    }
}

async fn bounded(mut response: reqwest::Response) -> Result<Zeroizing<Vec<u8>>, AuthError> {
    if !response.status().is_success() {
        return Err(AuthError::OAuthRejected);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err(AuthError::TooLarge);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(AuthError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_discovery(bytes: &[u8]) -> Result<(), AuthError> {
    let document: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| AuthError::Protocol)?;
    if document["issuer"].as_str() != Some(ISSUER)
        || document["revocation_endpoint"].as_str() != Some(REVOKE)
        || document["jwks_uri"].as_str() != Some(JWKS)
    {
        return Err(AuthError::Protocol);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
