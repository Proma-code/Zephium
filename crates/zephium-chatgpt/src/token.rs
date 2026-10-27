//! Protected OAuth credential records. Never expose these records through IPC.
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::oauth::AuthError;

/// Validated registration identity; an email alone does not identify a workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Exact OpenAI OIDC issuer.
    pub issuer: String,
    /// Verified OIDC subject.
    pub subject: String,
    /// Issued registration ID, never the dynamic bootstrap ID.
    pub client_id: String,
    /// Stable opaque identifier of this installation.
    pub host_id: String,
    /// Optional verified-token display label, not account identity.
    pub email: Option<String>,
}

/// One registration's protected, indivisible token set.
///
/// There is no public token constructor or Deserialize implementation. Only the
/// `from_bytes` restores protected vault records; new authorization is retired.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub(crate) version: u8,
    pub(crate) identity: Identity,
    pub(crate) access_token: Zeroizing<String>,
    pub(crate) refresh_token: Zeroizing<String>,
    pub(crate) id_token: Zeroizing<String>,
    pub(crate) nonce: Zeroizing<String>,
    pub(crate) scopes: Vec<String>,
    pub(crate) expires_at: u64,
}

/// Validated OAuth credentials. Debug output and serialization are explicit.
pub struct Tokens(pub(crate) Record);

impl std::fmt::Debug for Tokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Tokens([redacted])")
    }
}

impl Tokens {
    /// Validated, non-secret registration identity for native account selection.
    pub fn identity(&self) -> &Identity {
        &self.0.identity
    }

    /// Encode only for an atomic protected-vault write, never browser storage.
    #[cfg(test)]
    pub(crate) fn to_bytes(&self) -> Result<Zeroizing<Vec<u8>>, AuthError> {
        serde_json::to_vec(&self.0)
            .map(Zeroizing::new)
            .map_err(|_| AuthError::Protocol)
    }

    /// Restore a record from the app's integrity-protected native vault.
    ///
    /// This is not a token-import endpoint. The caller must authenticate the
    /// vault record and bind it to its selected profile before using it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AuthError> {
        if bytes.len() > 128 * 1024 {
            return Err(AuthError::TooLarge);
        }
        let record: Record = serde_json::from_slice(bytes).map_err(|_| AuthError::Protocol)?;
        if record.version != 1
            || !valid_identity(&record.identity)
            || !valid_secret(&record.access_token)
            || !valid_secret(&record.refresh_token)
            || !valid_secret(&record.id_token)
            || !valid_secret(&record.nonce)
            || record.expires_at == 0
            || !valid_scopes(&record.scopes)
        {
            return Err(AuthError::Protocol);
        }
        Ok(Self(record))
    }
}

pub(crate) fn valid_secret(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32 * 1024
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(crate) fn valid_client(value: &str) -> bool {
    value.starts_with("oaiapp_")
        && value.len() > 7
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn valid_host(value: &str) -> bool {
    let Some(uuid) = value.strip_prefix("urn:uuid:") else {
        return false;
    };
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
        })
}

pub(crate) fn valid_identity(identity: &Identity) -> bool {
    identity.issuer == "https://auth.openai.com"
        && valid_client(&identity.client_id)
        && valid_host(&identity.host_id)
        && !identity.subject.is_empty()
        && identity.subject.len() <= 512
        && !identity.subject.chars().any(char::is_control)
        && identity
            .email
            .as_ref()
            .is_none_or(|email| email.len() <= 320 && !email.chars().any(char::is_control))
}

pub(crate) fn valid_scopes(scopes: &[String]) -> bool {
    scopes.len() <= 64
        && scopes.iter().all(|scope| {
            !scope.is_empty()
                && scope.len() <= 128
                && scope.bytes().all(|byte| byte.is_ascii_graphic())
        })
        && ["openid", "resource.invoke", "chatgpt.tokens.use.direct"]
            .iter()
            .all(|required| scopes.iter().any(|scope| scope == required))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_and_host_ids_are_not_interchangeable() {
        assert!(valid_client("oaiapp_example"));
        assert!(!valid_client("dynamic_agent_client"));
        assert!(!valid_client("oaiapp_"));
        assert!(valid_host("urn:uuid:12345678-1234-4123-8123-123456789abc"));
        assert!(!valid_host("urn:uuid:12345678-1234-1123-8123-123456789abc"));
        assert!(!valid_host("urn:uuid:12345678-1234-4123-7123-123456789abc"));
    }

    #[test]
    fn identity_scopes_are_not_plan_usage_permission() {
        assert!(!valid_scopes(&["openid".into(), "email".into()]));
        assert!(valid_scopes(&[
            "openid".into(),
            "resource.invoke".into(),
            "chatgpt.tokens.use.direct".into()
        ]));
        assert!(Tokens::from_bytes(b"{}").is_err());
        assert!(Tokens::from_bytes(&vec![b' '; 128 * 1024 + 1]).is_err());
    }
}
