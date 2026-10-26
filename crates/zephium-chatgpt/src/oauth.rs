//! Native public-client Sign in with ChatGPT, using only official fixed endpoints.
//! No provider CLI credentials or browser cookies are imported.
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::Zeroizing;

use crate::token::{
    valid_client, valid_host, valid_identity, valid_scopes, valid_secret, Identity, Record, Tokens,
};

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const JWKS: &str = "https://auth.openai.com/.well-known/jwks.json";
const REVOKE: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const MAX_RESPONSE: usize = 128 * 1024;

/// Closed, secret-free errors for native account UX.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthError {
    /// A registration, host ID, or loopback address was invalid.
    Configuration,
    /// The operating system could not supply cryptographic randomness.
    Random,
    /// The callback URL or its parameters were malformed.
    Callback,
    /// The callback was not bound to this attempt.
    State,
    /// The person declined authorization.
    AccessDenied,
    /// Dynamic registration did not produce its own issued client ID.
    Registration,
    /// The request could not reach the trusted service.
    Transport,
    /// The bounded request deadline elapsed.
    Timeout,
    /// A response exceeded its byte budget.
    TooLarge,
    /// The trusted service returned an invalid response.
    Protocol,
    /// OAuth rejected the grant; start a new sign-in rather than replaying it.
    OAuthRejected,
    /// A renewable grant has expired or been revoked.
    RefreshExpired,
    /// A one-use rotation may have completed but no verified result is usable.
    /// Never replay the old refresh credential after this outcome.
    RefreshUncertain,
    /// The provider temporarily refused requests due to a limit.
    RateLimited,
    /// The returned grant does not authorize direct ChatGPT plan usage.
    Scope,
    /// An ID token failed signature, issuer, audience, expiry or nonce checks.
    IdToken,
    /// A returning login attempted to replace a different account registration.
    IdentityChanged,
    /// The local clock could not produce a valid timestamp.
    Clock,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for AuthError {}

/// Generate a UUID-v4 host identity. Persist it once before first authorization.
pub fn new_host_id() -> Result<String, AuthError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Random)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!("urn:uuid:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]))
}

fn rotation_error(error: AuthError) -> AuthError {
    match error {
        AuthError::RefreshExpired | AuthError::RateLimited => error,
        _ => AuthError::RefreshUncertain,
    }
}

fn random_secret() -> Result<Zeroizing<String>, AuthError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| AuthError::Random)?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_ref())))
}

/// One fresh PKCE authorization attempt, created only after native user consent.
///
/// The host must bind its loopback listener before construction, own a short
/// deadline, and discard the attempt on cancellation or profile switching.
pub struct LoginAttempt {
    host_id: String,
    returning: Option<Identity>,
    registered_client: Option<String>,
    redirect: String,
    state: Zeroizing<String>,
    nonce: Zeroizing<String>,
    verifier: Zeroizing<String>,
}

impl std::fmt::Debug for LoginAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LoginAttempt([redacted])")
    }
}

impl LoginAttempt {
    /// Prepare a new or returning registration on an already-bound loopback port.
    pub fn new(host_id: &str, returning: Option<&Identity>, port: u16) -> Result<Self, AuthError> {
        if port == 0 || !valid_host(host_id) {
            return Err(AuthError::Configuration);
        }
        if returning
            .is_some_and(|identity| !valid_identity(identity) || identity.host_id != host_id)
        {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            host_id: host_id.to_owned(),
            returning: returning.cloned(),
            registered_client: returning.map(|identity| identity.client_id.clone()),
            redirect: Url::parse(&format!("http://127.0.0.1:{port}/auth/callback"))
                .map_err(|_| AuthError::Configuration)?
                .into(),
            state: random_secret()?,
            nonce: random_secret()?,
            verifier: random_secret()?,
        })
    }

    /// Retry a callback-issued registration after code exchange failed.
    ///
    /// This ID is pending metadata, not a verified identity or active account.
    /// Accept it only from a previously state-validated native callback, never
    /// from renderer input. A fresh code and verified ID token are still required.
    pub fn retry_registration(
        host_id: &str,
        issued_client_id: &str,
        port: u16,
    ) -> Result<Self, AuthError> {
        if !valid_client(issued_client_id) {
            return Err(AuthError::Registration);
        }
        let mut attempt = Self::new(host_id, None, port)?;
        attempt.registered_client = Some(issued_client_id.to_owned());
        Ok(attempt)
    }

    /// Exact callback URI for the native listener, without codes or query values.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect
    }

    /// URL to open in the system browser. Never persist or log it.
    pub fn authorization_url(&self) -> Result<Zeroizing<String>, AuthError> {
        let mut url = Url::parse(AUTHORIZE).map_err(|_| AuthError::Configuration)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()));
        {
            let mut query = url.query_pairs_mut();
            query.append_pair(
                "client_id",
                self.registered_client
                    .as_deref()
                    .unwrap_or("dynamic_agent_client"),
            );
            if self.registered_client.is_none() {
                query.append_pair("agent_name_hint", "Zephium");
            }
            query
                .append_pair("ext_agent_host_id", &self.host_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &self.redirect)
                .append_pair("scope", SCOPES)
                .append_pair("resource", RESOURCE)
                .append_pair("state", &self.state)
                .append_pair("nonce", &self.nonce)
                .append_pair("code_challenge_method", "S256")
                .append_pair("code_challenge", &challenge);
        }
        Ok(Zeroizing::new(url.into()))
    }

    /// Check whether a request belongs to this listener's pending ceremony.
    /// Unrelated loopback requests do not consume or reveal the attempt's state.
    pub fn matches_callback(&self, callback_url: &str) -> bool {
        self.callback_parameters(callback_url).is_ok()
    }

    fn callback_parameters(
        &self,
        callback_url: &str,
    ) -> Result<BTreeMap<String, Zeroizing<String>>, AuthError> {
        if callback_url.len() > 8192
            || callback_url.chars().any(char::is_control)
            || callback_url.contains('\\')
        {
            return Err(AuthError::Callback);
        }
        let url = Url::parse(callback_url).map_err(|_| AuthError::Callback)?;
        let mut base = url.clone();
        base.set_query(None);
        if base.as_str() != self.redirect || url.fragment().is_some() {
            return Err(AuthError::Callback);
        }
        let mut parameters = BTreeMap::new();
        for (key, value) in url.query_pairs() {
            if parameters
                .insert(key.into_owned(), Zeroizing::new(value.into_owned()))
                .is_some()
            {
                return Err(AuthError::Callback);
            }
        }
        if !parameters
            .get("state")
            .is_some_and(|state| constant_time_eq(state, &self.state))
        {
            return Err(AuthError::State);
        }
        Ok(parameters)
    }

    /// Consume a validated, state-bound callback exactly once.
    pub fn accept_callback(self, callback_url: &str) -> Result<Authorization, AuthError> {
        let mut parameters = self.callback_parameters(callback_url)?;
        if let Some(error) = parameters.get("error") {
            return Err(if error.as_str() == "access_denied" {
                AuthError::AccessDenied
            } else {
                AuthError::OAuthRejected
            });
        }
        let client_id = match (&self.registered_client, parameters.get("client_id")) {
            (Some(expected), Some(client_id)) if client_id.as_str() != expected => {
                return Err(AuthError::IdentityChanged)
            }
            (Some(expected), _) => expected.clone(),
            (None, Some(client_id)) if valid_client(client_id) => client_id.to_string(),
            _ => return Err(AuthError::Registration),
        };
        let code = parameters
            .remove("code")
            .filter(|code| valid_secret(code))
            .ok_or(AuthError::Callback)?;
        Ok(Authorization {
            attempt: self,
            client_id,
            code,
        })
    }
}

/// A consumed callback bound to its own client, nonce, redirect URI and verifier.
/// No public constructor exists; only `LoginAttempt::accept_callback` creates it.
pub struct Authorization {
    attempt: LoginAttempt,
    client_id: String,
    code: Zeroizing<String>,
}

impl Authorization {
    /// Callback-issued registration ID. Retain as pending metadata if exchange
    /// fails; it is not verified account identity until `exchange` succeeds.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

impl std::fmt::Debug for Authorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Authorization([redacted])")
    }
}

/// Fixed-origin native OAuth HTTP client. Redirects and ambient proxies are off.
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

    /// Exchange this application's own code. Consuming it prevents code replay.
    pub async fn exchange(&self, authorization: Authorization) -> Result<Tokens, AuthError> {
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", authorization.client_id.as_str()),
            ("code", authorization.code.as_str()),
            ("code_verifier", authorization.attempt.verifier.as_str()),
            ("redirect_uri", authorization.attempt.redirect.as_str()),
            ("resource", RESOURCE),
        ];
        // Fetch verifier material before spending the one-use code.
        let jwks = self.jwks().await?;
        let response = self.post_token(&form, false).await?;
        let received = now()?;
        tokens_from_response(response, &authorization, &jwks, received)
    }

    /// Refresh one session. The caller serializes this and atomically stores the
    /// complete replacement before allowing any concurrent account operation.
    pub async fn refresh(&self, tokens: &Tokens) -> Result<Tokens, AuthError> {
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", tokens.0.identity.client_id.as_str()),
            ("refresh_token", tokens.0.refresh_token.as_str()),
            ("resource", RESOURCE),
        ];
        // Never introduce a new network dependency after consuming a rotating
        // refresh token. A failed preflight leaves the old grant unspent.
        let jwks = self.jwks().await?;
        let response = self.post_token(&form, true).await.map_err(rotation_error)?;
        let received = now().map_err(|_| AuthError::RefreshUncertain)?;
        refreshed_tokens(response, tokens, Some(jwks.as_slice()), received)
            .map_err(|_| AuthError::RefreshUncertain)
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
            Err(status_error(response.status(), false))
        }
    }

    async fn jwks(&self) -> Result<Zeroizing<Vec<u8>>, AuthError> {
        bounded(self.http.get(JWKS).send().await.map_err(http_error)?).await
    }

    async fn post_token(
        &self,
        form: &[(&str, &str)],
        refresh: bool,
    ) -> Result<TokenResponse, AuthError> {
        let response = self
            .http
            .post(TOKEN)
            .form(form)
            .send()
            .await
            .map_err(http_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(status_error(status, refresh));
        }
        let body = bounded(response).await?;
        serde_json::from_slice(&body).map_err(|_| AuthError::Protocol)
    }
}

fn http_error(error: reqwest::Error) -> AuthError {
    if error.is_timeout() {
        AuthError::Timeout
    } else {
        AuthError::Transport
    }
}

fn status_error(status: reqwest::StatusCode, refresh: bool) -> AuthError {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        AuthError::RateLimited
    } else if status.is_server_error() {
        AuthError::Transport
    } else if refresh && matches!(status.as_u16(), 400 | 401) {
        AuthError::RefreshExpired
    } else {
        AuthError::OAuthRejected
    }
}

async fn bounded(mut response: reqwest::Response) -> Result<Zeroizing<Vec<u8>>, AuthError> {
    if !response.status().is_success() {
        return Err(status_error(response.status(), false));
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

fn now() -> Result<u64, AuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| AuthError::Clock)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Zeroizing<String>,
    refresh_token: Option<Zeroizing<String>>,
    id_token: Option<Zeroizing<String>>,
    token_type: String,
    expires_in: u64,
    scope: Option<String>,
}

fn token_metadata(
    response: &TokenResponse,
    previous: Option<&Tokens>,
    received: u64,
) -> Result<(Vec<String>, u64), AuthError> {
    if !response.token_type.eq_ignore_ascii_case("bearer")
        || !valid_secret(&response.access_token)
        || response.expires_in == 0
        || response.expires_in > 31 * 24 * 60 * 60
    {
        return Err(AuthError::Protocol);
    }
    let scopes: Vec<String> = match &response.scope {
        Some(scope) => scope.split_ascii_whitespace().map(str::to_owned).collect(),
        None => previous
            .map(|tokens| tokens.0.scopes.clone())
            .ok_or(AuthError::Scope)?,
    };
    if !valid_scopes(&scopes) {
        return Err(AuthError::Scope);
    }
    let expires_at = received
        .checked_add(response.expires_in)
        .ok_or(AuthError::Clock)?;
    Ok((scopes, expires_at))
}

fn tokens_from_response(
    response: TokenResponse,
    authorization: &Authorization,
    jwks: &[u8],
    received: u64,
) -> Result<Tokens, AuthError> {
    let (scopes, expires_at) = token_metadata(&response, None, received)?;
    let id_token = response.id_token.ok_or(AuthError::IdToken)?;
    let identity = validate_id_token(
        &id_token,
        jwks,
        &authorization.client_id,
        &authorization.attempt.host_id,
        NonceCheck {
            expected: &authorization.attempt.nonce,
            required: true,
        },
        authorization.attempt.returning.as_ref(),
        received,
    )?;
    let refresh_token = response
        .refresh_token
        .filter(|token| valid_secret(token))
        .ok_or(AuthError::Protocol)?;
    Ok(Tokens(Record {
        version: 1,
        identity,
        access_token: response.access_token,
        refresh_token,
        id_token,
        nonce: authorization.attempt.nonce.clone(),
        scopes,
        expires_at,
    }))
}

fn refreshed_tokens(
    response: TokenResponse,
    previous: &Tokens,
    jwks: Option<&[u8]>,
    received: u64,
) -> Result<Tokens, AuthError> {
    let (scopes, expires_at) = token_metadata(&response, Some(previous), received)?;
    let (id_token, identity) = if let Some(id_token) = response.id_token {
        let identity = validate_id_token(
            &id_token,
            jwks.ok_or(AuthError::IdToken)?,
            &previous.0.identity.client_id,
            &previous.0.identity.host_id,
            NonceCheck {
                expected: &previous.0.nonce,
                required: false,
            },
            Some(&previous.0.identity),
            received,
        )?;
        (id_token, identity)
    } else {
        (previous.0.id_token.clone(), previous.0.identity.clone())
    };
    let refresh_token = response
        .refresh_token
        .unwrap_or_else(|| previous.0.refresh_token.clone());
    if !valid_secret(&refresh_token) {
        return Err(AuthError::Protocol);
    }
    Ok(Tokens(Record {
        version: 1,
        identity,
        access_token: response.access_token,
        refresh_token,
        id_token,
        nonce: previous.0.nonce.clone(),
        scopes,
        expires_at,
    }))
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
    iat: u64,
    aud: serde_json::Value,
    azp: Option<String>,
    nonce: Option<Zeroizing<String>>,
    email: Option<String>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    use_: Option<String>,
    n: String,
    e: String,
}

struct NonceCheck<'a> {
    expected: &'a str,
    required: bool,
}

fn validate_id_token(
    token: &str,
    jwks: &[u8],
    client_id: &str,
    host_id: &str,
    nonce: NonceCheck<'_>,
    previous: Option<&Identity>,
    received: u64,
) -> Result<Identity, AuthError> {
    if !valid_secret(token) || jwks.len() > MAX_RESPONSE {
        return Err(AuthError::IdToken);
    }
    let header = decode_header(token).map_err(|_| AuthError::IdToken)?;
    if header.alg != Algorithm::RS256
        || header.jku.is_some()
        || header.jwk.is_some()
        || header.x5u.is_some()
        || header
            .crit
            .as_ref()
            .is_some_and(|critical| !critical.is_empty())
    {
        return Err(AuthError::IdToken);
    }
    let kid = header
        .kid
        .filter(|kid| !kid.is_empty() && kid.len() <= 256)
        .ok_or(AuthError::IdToken)?;
    let set: Jwks = serde_json::from_slice(jwks).map_err(|_| AuthError::IdToken)?;
    if set.keys.is_empty() || set.keys.len() > 16 {
        return Err(AuthError::IdToken);
    }
    let mut keys = set
        .keys
        .into_iter()
        .filter(|key| key.get("kid").and_then(serde_json::Value::as_str) == Some(&kid));
    let key: Jwk = serde_json::from_value(keys.next().ok_or(AuthError::IdToken)?)
        .map_err(|_| AuthError::IdToken)?;
    if keys.next().is_some()
        || key.kid != kid
        || key.kty != "RSA"
        || key.alg.as_deref().is_some_and(|alg| alg != "RS256")
        || key.use_.as_deref().is_some_and(|use_| use_ != "sig")
    {
        return Err(AuthError::IdToken);
    }
    let decoding =
        DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|_| AuthError::IdToken)?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
    validation.leeway = 30;
    validation.validate_nbf = true;
    let claims = decode::<Claims>(token, &decoding, &validation)
        .map_err(|_| AuthError::IdToken)?
        .claims;
    if claims.iat == 0
        || claims.iat > received.saturating_add(30)
        || claims.exp <= claims.iat
        || claims.exp <= received.saturating_sub(30)
    {
        return Err(AuthError::IdToken);
    }
    match claims.nonce.as_deref() {
        Some(actual) if !constant_time_eq(actual, nonce.expected) => {
            return Err(AuthError::IdToken)
        }
        None if nonce.required => return Err(AuthError::IdToken),
        _ => {}
    }
    if let Some(audiences) = claims.aud.as_array() {
        if audiences.len() > 1 && claims.azp.as_deref() != Some(client_id) {
            return Err(AuthError::IdToken);
        }
    }
    if claims
        .azp
        .as_deref()
        .is_some_and(|authorized| authorized != client_id)
    {
        return Err(AuthError::IdToken);
    }
    let identity = Identity {
        issuer: ISSUER.into(),
        subject: claims.sub,
        client_id: client_id.into(),
        host_id: host_id.into(),
        email: claims.email,
    };
    if !valid_identity(&identity) {
        return Err(AuthError::IdToken);
    }
    if previous.is_some_and(|previous| {
        previous.subject != identity.subject
            || previous.client_id != identity.client_id
            || previous.issuer != identity.issuer
            || previous.host_id != identity.host_id
    }) {
        return Err(AuthError::IdentityChanged);
    }
    Ok(identity)
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
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::{json, Value};

    const HOST: &str = "urn:uuid:12345678-1234-4123-8123-123456789abc";
    const CLIENT: &str = "oaiapp_test";
    // Synthetic test-only RSA key. It is not used by a server or account.
    const PRIVATE_DER: &str = "MIIEpAIBAAKCAQEAmbaY3jzBFbMniBdH2E+pRCffvGVDAnuI6F05FjGCItM9jc2auZlN0rECD2dtJ+SpNUPW5a8qiu3UyIwTUGmqQ+SoN+4TzBDTOEkBhq/V01SiQS6FjjI8IIs9xsb86KkFZyBdWNHaCpxSQQPD9mrpRkWMSZv1hGTkTHB87hi+IV2dTMFoH93z5k3bPXFSmqi0u6ZxfuwtVEEMGgFpYeQomWwgLQqcCiIJp1X4F1+xmdfnrCdmrHP1AqR2+0AAKyjiHV1043p6ucuKEesmb0Bm3H3GjS7isuaBXBsUmvZdIm5BHbUXuRYCZEr/hJJ8ldZahM+S4BsXJ2Rl4Ljzx+rYdQIDAQABAoIBACTFvk3iNmXo0VL5EM6yhgGgUWG6TtzT7VV7YcoJp4KJyv62umZewrOd9Wp2eXJS/VjZBRq2/EQ2r9IXUpKI6Lrrw4N30OQffUxIiNUuEQcIxvU1ei1hnqlBblK0rXHCZ6C7aSFzuIgTzAiyLCnx+yL8xdohNYvJ8CTKP1TddfS4l3mxxrzDUQ3eXTMxlliy7VpFZQzAcwYc2doAqrWV4AD/mTXKxPwN+ayxhDRzoIPOnpWhZ7YkgfS15Yw/SzIL3274ee7OztX2urR7xtG4D5ysapqrcmh2kiGIQL6pAGW2Yd52JeB5YKTfu0bkguz2fSGvpo9ace8epTyW21DfOMkCgYEA2RgtApKjMU11M6WOjCKsXgQKPRB66SazZsdvllRUknePWIhkpPVYLNtuEnb30W6lYmhZ/My+FqxPWS/2GvPhF8jFKd4AWpxDrcQ20PhgKIn6mSM4Qiqm4EubSLgBA6Cii4FeHGgzz+9yT1Am4kDjgpfkBvDWalRBjhgUMFZ067kCgYEAtUKhVHFowpX7ZW0AvcC6ZmGMLFzhngHimlDG/G/oxTzQlVATcCvKr/n8RKyoqse6FqcKzoJBbvlsJ9JJtm391eKkCvoZtVC5Hcn74Tb3/ux0phdeoiJFKbXuiwT+slmk0nYcWWbqH2F8tEKlUYH+7+ix0mZLMIMJO/ZpE6LciJ0CgYEAkatWMF7CoAdweCpDvyZY8HDXiefwAVgVkwaJip6lbO5KwkYq+kEiXF8UudEYMnCDCsgocafUeRQ/3gF/qtAwHqx6F8C+Dg7CnuuFOK0goLMM8wqd6lACT4iL9rx4+b0ktGUnB/qnoYPy+OM4ZCLURkGzITEjPo+wuS7RBSpOJAkCgYBj5wJ2QT+FKBb0Uvy6mPPjo9YXSWYMqWtUtDrIPYHk29z2vrjH9Ur0bOruit83qXdWLuEU6tPXr18+H3gDNWJXNWBGsIvk1EYDNiU+fwZ2maq9NFozMIJAOAbu1mTutdvTJ8f+OmvFufx6edUWNHjnj7Aaj8jXRxGa2/gUG4pDPQKBgQCkHUfhy4Sg+9rmIOCUsu2jyVO4jYF2OMVwmAlURZxz6X3voENjxj40giwcJN3mNQO/5RXQGeco9TUwszi3Mw08eXb8oj+YYtRHg1ZURTTdToFIODaGlvu6ho0BcvhDbA/yf2XEiDpGCsZulIBeHCUGXbFKk9IrXhvwema2H+Lb1g==";
    const MODULUS: &str = "mbaY3jzBFbMniBdH2E-pRCffvGVDAnuI6F05FjGCItM9jc2auZlN0rECD2dtJ-SpNUPW5a8qiu3UyIwTUGmqQ-SoN-4TzBDTOEkBhq_V01SiQS6FjjI8IIs9xsb86KkFZyBdWNHaCpxSQQPD9mrpRkWMSZv1hGTkTHB87hi-IV2dTMFoH93z5k3bPXFSmqi0u6ZxfuwtVEEMGgFpYeQomWwgLQqcCiIJp1X4F1-xmdfnrCdmrHP1AqR2-0AAKyjiHV1043p6ucuKEesmb0Bm3H3GjS7isuaBXBsUmvZdIm5BHbUXuRYCZEr_hJJ8ldZahM-S4BsXJ2Rl4Ljzx-rYdQ";

    fn identity() -> Identity {
        Identity {
            issuer: ISSUER.into(),
            subject: "subject-one".into(),
            client_id: CLIENT.into(),
            host_id: HOST.into(),
            email: Some("test@example.invalid".into()),
        }
    }

    fn callback(attempt: &LoginAttempt, client: Option<&str>) -> String {
        let mut url = Url::parse(attempt.redirect_uri()).unwrap();
        url.query_pairs_mut()
            .append_pair("code", "test-code")
            .append_pair("state", &attempt.state);
        if let Some(client) = client {
            url.query_pairs_mut().append_pair("client_id", client);
        }
        url.into()
    }

    fn authorization() -> Authorization {
        let attempt = LoginAttempt::new(HOST, None, 45678).unwrap();
        let callback_url = callback(&attempt, Some(CLIENT));
        attempt.accept_callback(&callback_url).unwrap()
    }

    fn jwks() -> Vec<u8> {
        json!({"keys": [{"kid": "test-key", "kty": "RSA", "alg": "RS256", "use": "sig", "n": MODULUS, "e": "AQAB"}]}).to_string().into_bytes()
    }

    fn claims(nonce: &str) -> Value {
        let time = now().unwrap();
        json!({"iss": ISSUER, "sub": "subject-one", "aud": CLIENT, "exp": time + 3600, "iat": time, "nonce": nonce, "email": "test@example.invalid"})
    }

    fn sign(claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-key".into());
        encode(
            &header,
            claims,
            &EncodingKey::from_rsa_der(&STANDARD.decode(PRIVATE_DER).unwrap()),
        )
        .unwrap()
    }

    fn validate(token: &str, nonce: &str) -> Result<Identity, AuthError> {
        validate_id_token(
            token,
            &jwks(),
            CLIENT,
            HOST,
            NonceCheck {
                expected: nonce,
                required: true,
            },
            None,
            now().unwrap(),
        )
    }

    fn response(token: String) -> TokenResponse {
        TokenResponse {
            access_token: Zeroizing::new("test-access".into()),
            refresh_token: Some(Zeroizing::new("test-refresh".into())),
            id_token: Some(Zeroizing::new(token)),
            token_type: "Bearer".into(),
            expires_in: 3600,
            scope: Some(SCOPES.into()),
        }
    }

    fn tokens() -> Tokens {
        let authorization = authorization();
        let token = sign(&claims(&authorization.attempt.nonce));
        tokens_from_response(response(token), &authorization, &jwks(), now().unwrap()).unwrap()
    }

    #[test]
    fn authorization_uses_own_name_fresh_entropy_and_pkce_s256() {
        let mut first = LoginAttempt::new(HOST, None, 45678).unwrap();
        let second = LoginAttempt::new(HOST, None, 45678).unwrap();
        assert_ne!(*first.state, *second.state);
        assert_ne!(*first.nonce, *second.nonce);
        assert_ne!(*first.verifier, *second.verifier);
        assert_eq!(first.verifier.len(), 43);
        first.verifier = Zeroizing::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        let url = Url::parse(&first.authorization_url().unwrap()).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.origin().ascii_serialization(), ISSUER);
        assert_eq!(query["client_id"], "dynamic_agent_client");
        assert_eq!(query["agent_name_hint"], "Zephium");
        assert_eq!(query["redirect_uri"], first.redirect_uri());
        assert_eq!(query["scope"], SCOPES);
        assert_eq!(query["resource"], RESOURCE);
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(
            query["code_challenge"],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(!query.contains_key("client_secret"));
        assert!(!format!("{first:?}").contains(&*first.state));
        let host = new_host_id().unwrap();
        assert!(valid_host(&host));
        assert_ne!(host, new_host_id().unwrap());
    }

    #[test]
    fn returning_registration_reuses_exact_client_and_identity() {
        let selected = identity();
        let attempt = LoginAttempt::new(HOST, Some(&selected), 45678).unwrap();
        let url = Url::parse(&attempt.authorization_url().unwrap()).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], CLIENT);
        assert!(!query.contains_key("agent_name_hint"));
        let callback_url = callback(&attempt, None);
        assert_eq!(
            attempt.accept_callback(&callback_url).unwrap().client_id,
            CLIENT
        );
        let attempt = LoginAttempt::new(HOST, Some(&selected), 45678).unwrap();
        let callback_url = callback(&attempt, Some("oaiapp_changed"));
        assert_eq!(
            attempt.accept_callback(&callback_url).unwrap_err(),
            AuthError::IdentityChanged
        );
    }

    #[test]
    fn failed_exchange_retry_retains_client_but_never_invents_an_identity() {
        let authorization = authorization();
        let attempt =
            LoginAttempt::retry_registration(HOST, authorization.client_id(), 45679).unwrap();
        assert!(attempt.returning.is_none());
        let url = Url::parse(&attempt.authorization_url().unwrap()).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], CLIENT);
        assert!(!query.contains_key("agent_name_hint"));
        assert_ne!(*attempt.state, *authorization.attempt.state);
        let callback_url = callback(&attempt, None);
        assert_eq!(
            attempt.accept_callback(&callback_url).unwrap().client_id(),
            CLIENT
        );
        assert!(LoginAttempt::retry_registration(HOST, "dynamic_agent_client", 45678).is_err());
    }

    #[test]
    fn callback_requires_exact_origin_path_state_and_issued_client() {
        for transform in [
            |url: String| url.replace("127.0.0.1", "localhost"),
            |url: String| url.replace("/auth/callback", "/callback"),
            |url: String| url.replace(":45678", ":45679"),
            |url: String| format!("{url}#fragment"),
            |url: String| format!("{url}&code=duplicate"),
        ] {
            let attempt = LoginAttempt::new(HOST, None, 45678).unwrap();
            let url = transform(callback(&attempt, Some(CLIENT)));
            assert_eq!(
                attempt.accept_callback(&url).unwrap_err(),
                AuthError::Callback
            );
        }
        let attempt = LoginAttempt::new(HOST, None, 45678).unwrap();
        let url = callback(&attempt, Some(CLIENT)).replace(&*attempt.state, "wrong");
        assert_eq!(attempt.accept_callback(&url).unwrap_err(), AuthError::State);
        for client in [None, Some("dynamic_agent_client")] {
            let attempt = LoginAttempt::new(HOST, None, 45678).unwrap();
            let url = callback(&attempt, client);
            assert_eq!(
                attempt.accept_callback(&url).unwrap_err(),
                AuthError::Registration
            );
        }
    }

    #[test]
    fn callback_prefilter_is_repeatable_and_does_not_consume_the_attempt() {
        let attempt = LoginAttempt::new(HOST, None, 45678).unwrap();
        let valid = callback(&attempt, Some(CLIENT));
        assert!(!attempt.matches_callback("http://127.0.0.1:45678/favicon.ico"));
        assert!(!attempt.matches_callback(&valid.replace(&*attempt.state, "unrelated")));
        assert!(attempt.matches_callback(&valid));
        assert!(attempt.matches_callback(&valid));
        assert_eq!(attempt.accept_callback(&valid).unwrap().client_id(), CLIENT);
    }

    #[test]
    fn access_denied_is_bound_to_state_before_processing() {
        let attempt = LoginAttempt::new(HOST, None, 45678).unwrap();
        let url = format!("{}?error=access_denied&state=wrong", attempt.redirect_uri());
        assert_eq!(attempt.accept_callback(&url).unwrap_err(), AuthError::State);
        let attempt = LoginAttempt::new(HOST, None, 45678).unwrap();
        let url = format!(
            "{}?error=access_denied&state={}",
            attempt.redirect_uri(),
            attempt.state.as_str()
        );
        assert_eq!(
            attempt.accept_callback(&url).unwrap_err(),
            AuthError::AccessDenied
        );
    }

    #[test]
    fn signed_identity_checks_issuer_audience_nonce_expiry_and_issued_at() {
        assert_eq!(
            validate(&sign(&claims("nonce")), "nonce").unwrap(),
            identity()
        );
        for (field, value) in [
            ("iss", json!("https://evil.example")),
            ("aud", json!("oaiapp_other")),
            ("nonce", json!("different")),
            ("exp", json!(now().unwrap() - 100)),
            ("iat", json!(now().unwrap() + 3600)),
            ("sub", json!("")),
        ] {
            let mut value_claims = claims("nonce");
            value_claims[field] = value;
            assert_eq!(
                validate(&sign(&value_claims), "nonce").unwrap_err(),
                AuthError::IdToken,
                "{field}"
            );
        }
        let mut missing = claims("nonce");
        missing.as_object_mut().unwrap().remove("nonce");
        assert_eq!(
            validate(&sign(&missing), "nonce").unwrap_err(),
            AuthError::IdToken
        );
    }

    #[test]
    fn altered_signature_untrusted_key_urls_and_algorithm_confusion_fail() {
        let mut corrupted = sign(&claims("nonce"));
        let signature_start = corrupted.rfind('.').unwrap() + 1;
        corrupted.replace_range(signature_start..signature_start + 1, "!");
        assert_eq!(
            validate(&corrupted, "nonce").unwrap_err(),
            AuthError::IdToken
        );
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-key".into());
        header.jku = Some("https://evil.example/keys".into());
        let token = encode(
            &header,
            &claims("nonce"),
            &EncodingKey::from_rsa_der(&STANDARD.decode(PRIVATE_DER).unwrap()),
        )
        .unwrap();
        assert_eq!(validate(&token, "nonce").unwrap_err(), AuthError::IdToken);
        let token = encode(
            &Header::default(),
            &claims("nonce"),
            &EncodingKey::from_secret(b"test"),
        )
        .unwrap();
        assert_eq!(validate(&token, "nonce").unwrap_err(), AuthError::IdToken);
    }

    #[test]
    fn jwks_selection_rejects_ambiguous_keys_but_ignores_unrelated_algorithms() {
        let token = sign(&claims("nonce"));
        let mut set: Value = serde_json::from_slice(&jwks()).unwrap();
        let duplicate = set["keys"][0].clone();
        set["keys"].as_array_mut().unwrap().push(duplicate);
        assert_eq!(
            validate_id_token(
                &token,
                set.to_string().as_bytes(),
                CLIENT,
                HOST,
                NonceCheck {
                    expected: "nonce",
                    required: true
                },
                None,
                now().unwrap()
            )
            .unwrap_err(),
            AuthError::IdToken
        );
        set["keys"].as_array_mut().unwrap().pop();
        set["keys"].as_array_mut().unwrap().push(json!({"kid": "other", "kty": "EC", "crv": "P-256", "x": "irrelevant", "y": "irrelevant"}));
        assert!(validate_id_token(
            &token,
            set.to_string().as_bytes(),
            CLIENT,
            HOST,
            NonceCheck {
                expected: "nonce",
                required: true
            },
            None,
            now().unwrap()
        )
        .is_ok());
    }

    #[test]
    fn multiple_audiences_require_exact_authorized_party() {
        let mut value = claims("nonce");
        value["aud"] = json!([CLIENT, "another"]);
        assert_eq!(
            validate(&sign(&value), "nonce").unwrap_err(),
            AuthError::IdToken
        );
        value["azp"] = json!(CLIENT);
        assert!(validate(&sign(&value), "nonce").is_ok());
        value["azp"] = json!("another");
        assert_eq!(
            validate(&sign(&value), "nonce").unwrap_err(),
            AuthError::IdToken
        );
    }

    #[test]
    fn grant_scope_is_not_inferred_from_login_request() {
        let authorization = authorization();
        let mut response = response(sign(&claims(&authorization.attempt.nonce)));
        response.scope = Some("openid email".into());
        assert_eq!(
            tokens_from_response(response, &authorization, &jwks(), now().unwrap()).unwrap_err(),
            AuthError::Scope
        );
    }

    #[test]
    fn returning_login_cannot_replace_a_different_subject() {
        let attempt = LoginAttempt::new(HOST, Some(&identity()), 45678).unwrap();
        let url = callback(&attempt, None);
        let authorization = attempt.accept_callback(&url).unwrap();
        let mut value = claims(&authorization.attempt.nonce);
        value["sub"] = json!("someone-else");
        assert_eq!(
            tokens_from_response(
                response(sign(&value)),
                &authorization,
                &jwks(),
                now().unwrap()
            )
            .unwrap_err(),
            AuthError::IdentityChanged
        );
    }

    #[test]
    fn rotating_refresh_and_vault_roundtrip_keep_one_registration() {
        let previous = tokens();
        let mut response = response(String::new());
        response.access_token = Zeroizing::new("replacement-access".into());
        response.refresh_token = Some(Zeroizing::new("replacement-refresh".into()));
        response.id_token = None;
        response.scope = None;
        let next = refreshed_tokens(response, &previous, None, now().unwrap()).unwrap();
        assert_eq!(next.identity(), previous.identity());
        assert_eq!(previous.access_token(), "test-access");
        assert_eq!(next.access_token(), "replacement-access");
        assert_eq!(next.0.refresh_token.as_str(), "replacement-refresh");
        let encoded = next.to_bytes().unwrap();
        let restored = Tokens::from_bytes(&encoded).unwrap();
        assert_eq!(restored.identity(), next.identity());
        assert_eq!(restored.access_token(), next.access_token());
        assert!(!format!("{next:?}").contains("replacement"));
    }

    #[test]
    fn refresh_checks_returned_nonce_identity_and_reduced_scope() {
        let previous = tokens();
        let wrong_nonce = response(sign(&claims("different")));
        assert_eq!(
            refreshed_tokens(wrong_nonce, &previous, Some(&jwks()), now().unwrap()).unwrap_err(),
            AuthError::IdToken
        );
        let mut reduced = response(String::new());
        reduced.id_token = None;
        reduced.scope = Some("openid".into());
        assert_eq!(
            refreshed_tokens(reduced, &previous, None, now().unwrap()).unwrap_err(),
            AuthError::Scope
        );
        let mut without_nonce = claims("nonce");
        without_nonce.as_object_mut().unwrap().remove("nonce");
        assert!(refreshed_tokens(
            response(sign(&without_nonce)),
            &previous,
            Some(&jwks()),
            now().unwrap()
        )
        .is_ok());
    }

    #[test]
    fn revocation_discovery_cannot_redirect_tokens_to_another_origin() {
        let valid = json!({"issuer": ISSUER, "revocation_endpoint": REVOKE, "jwks_uri": JWKS});
        assert!(validate_discovery(valid.to_string().as_bytes()).is_ok());
        for endpoint in [
            "https://evil.example/revoke",
            "http://auth.openai.com/api/accounts/oauth/revoke",
            "https://auth.openai.com.evil.example/revoke",
        ] {
            let mut document = valid.clone();
            document["revocation_endpoint"] = json!(endpoint);
            assert_eq!(
                validate_discovery(document.to_string().as_bytes()).unwrap_err(),
                AuthError::Protocol
            );
        }
    }
    #[test]
    fn uncertain_rotation_never_authorizes_replaying_old_refresh() {
        for error in [
            AuthError::Transport,
            AuthError::Timeout,
            AuthError::Protocol,
            AuthError::IdToken,
            AuthError::Scope,
        ] {
            assert_eq!(rotation_error(error), AuthError::RefreshUncertain);
        }
        assert_eq!(
            rotation_error(AuthError::RateLimited),
            AuthError::RateLimited
        );
        let source = include_str!("oauth.rs");
        let refresh = source
            .split("pub async fn refresh(&self")
            .nth(1)
            .unwrap()
            .split("/// Attempt to revoke")
            .next()
            .unwrap();
        assert!(refresh.find("self.jwks()").unwrap() < refresh.find("self.post_token(").unwrap());
    }
}
