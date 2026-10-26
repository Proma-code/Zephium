//! Strict receipt for one fresh, uncredentialed debug fixture, not account discovery.
use crate::{protocol, strict_json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub(crate) enum Rejected {
    Protocol = 1,
    UnexpectedFields = 2,
    UnexpectedProvider = 3,
}

pub(crate) fn validate(stdout: &[u8], exit: i32, expected_directory: &str) -> Result<(), Rejected> {
    if protocol::parse_status(stdout, exit, expected_directory)
        != Ok(protocol::AuthStatus::SignedOut)
    {
        return Err(Rejected::Protocol);
    }
    let value = strict_json::parse(stdout).map_err(|_| Rejected::Protocol)?;
    let object = value.as_object().ok_or(Rejected::Protocol)?;
    // The reference does not promise a complete optional identity-field schema.
    // Unknown keys fail, including null identity fields, rather than admitting
    // arbitrary account material based on a credential-looking name heuristic.
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "loggedIn" | "authMethod" | "configDirectory" | "apiProvider"
        )
    }) {
        return Err(Rejected::UnexpectedFields);
    }
    if object
        .get("apiProvider")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some("firstParty"))
    {
        return Err(Rejected::UnexpectedProvider);
    }
    Ok(())
}

/// Content-free shape diagnostics only. They do not change schema acceptance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Shape {
    pub(crate) extra_keys: i32,
    pub(crate) extra_nulls: i32,
    pub(crate) known_extra: i32,
    pub(crate) identity_fields: i32,
}
pub(crate) fn shape(stdout: &[u8], expected_projects: &str) -> Option<Shape> {
    if stdout.len() > protocol::MAX_STATUS_BYTES {
        return None;
    }
    let value = strict_json::parse(stdout).ok()?;
    let object = value.as_object()?;
    let extras = object.iter().filter(|(key, _)| {
        !matches!(
            key.as_str(),
            "loggedIn" | "authMethod" | "configDirectory" | "apiProvider"
        )
    });
    let extra_keys = extras.clone().count() as i32;
    let extra_nulls = extras.filter(|(_, value)| value.is_null()).count() as i32;
    let analytics = object.get("analyticsDisabled");
    let projects = object.get("projectsDirectory");
    let facts = [
        analytics.is_some(),
        analytics.is_some_and(serde_json::Value::is_boolean),
        analytics.and_then(serde_json::Value::as_bool) == Some(true),
        projects.is_some(),
        projects.is_some_and(serde_json::Value::is_string),
        !expected_projects.is_empty()
            && projects.and_then(serde_json::Value::as_str) == Some(expected_projects),
    ];
    let known_extra = facts
        .into_iter()
        .enumerate()
        .fold(0, |bits, (i, yes)| bits | (i32::from(yes) << i));
    let identity_fields = ["email", "orgId", "orgName", "subscriptionType"]
        .into_iter()
        .enumerate()
        .fold(0, |bits, (i, key)| {
            bits | (i32::from(object.contains_key(key)) << i)
        });
    Some(Shape {
        extra_keys,
        extra_nulls,
        known_extra,
        identity_fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signed_out() -> serde_json::Value {
        json!({"loggedIn":false,"authMethod":"none","configDirectory":"synthetic-owned"})
    }
    #[test]
    fn shape_reports_only_fixed_booleans_counts_and_never_relaxes_validation() {
        let mut value = signed_out();
        value["analyticsDisabled"] = json!(true);
        value["projectsDirectory"] = json!("synthetic-owned/projects");
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            shape(&bytes, "synthetic-owned/projects"),
            Some(Shape {
                extra_keys: 2,
                extra_nulls: 0,
                known_extra: 63,
                identity_fields: 0
            })
        );
        assert_eq!(
            validate(&bytes, 1, "synthetic-owned"),
            Err(Rejected::UnexpectedFields)
        );
        assert_eq!(
            shape(&bytes, "other-profile/projects").unwrap().known_extra,
            31
        );
        value["email"] = json!("synthetic-never-print");
        value["synthetic-key-never-print"] = serde_json::Value::Null;
        let facts = shape(
            &serde_json::to_vec(&value).unwrap(),
            "synthetic-owned/projects",
        )
        .unwrap();
        assert_eq!(facts.extra_keys, 4);
        assert_eq!(facts.extra_nulls, 1);
        assert_eq!(facts.identity_fields, 1);
        assert!(!format!("{facts:?}").contains("synthetic"));
        assert_eq!(
            shape(&vec![b' '; protocol::MAX_STATUS_BYTES + 1], "synthetic"),
            None
        );
    }
    #[test]
    fn accepts_only_bound_signed_out_receipt() {
        let mut value = signed_out();
        for provider in [
            None,
            Some(serde_json::Value::Null),
            Some(json!("firstParty")),
        ] {
            if let Some(provider) = provider {
                value["apiProvider"] = provider;
            }
            let bytes = serde_json::to_vec(&value).unwrap();
            assert_eq!(validate(&bytes, 1, "synthetic-owned"), Ok(()));
            assert_eq!(
                validate(&bytes, 0, "synthetic-owned"),
                Err(Rejected::Protocol)
            );
            assert_eq!(
                validate(&bytes, 1, "other-profile"),
                Err(Rejected::Protocol)
            );
        }
    }
    #[test]
    fn unknown_and_identity_fields_fail_even_when_empty() {
        for key in [
            "email",
            "orgId",
            "account",
            "subscriptionType",
            "credential",
            "unknown",
        ] {
            for extra in [
                serde_json::Value::Null,
                json!(""),
                json!({}),
                json!([]),
                json!("synthetic"),
            ] {
                let mut value = signed_out();
                value[key] = extra;
                assert_eq!(
                    validate(&serde_json::to_vec(&value).unwrap(), 1, "synthetic-owned"),
                    Err(Rejected::UnexpectedFields)
                );
            }
        }
    }
    #[test]
    fn provider_or_authenticated_shapes_are_not_fresh_empty() {
        for provider in [json!("thirdParty"), json!(""), json!({}), json!(true)] {
            let mut value = signed_out();
            value["apiProvider"] = provider;
            assert_eq!(
                validate(&serde_json::to_vec(&value).unwrap(), 1, "synthetic-owned"),
                Err(Rejected::UnexpectedProvider)
            );
        }
        for method in [
            "claude.ai",
            "api_key",
            "api_key_helper",
            "third_party",
            "oauth_token",
        ] {
            let mut value = signed_out();
            value["loggedIn"] = json!(true);
            value["authMethod"] = json!(method);
            assert_eq!(
                validate(&serde_json::to_vec(&value).unwrap(), 0, "synthetic-owned"),
                Err(Rejected::Protocol)
            );
        }
    }
    #[test]
    fn duplicate_oversized_and_non_json_output_fail_closed() {
        for bytes in [
            b"{\"loggedIn\":false,\"loggedIn\":true}".as_slice(),
            b"https://claude.ai/synthetic",
            b"Paste code here >",
            b"{} {}",
            &[255],
        ] {
            assert_eq!(
                validate(bytes, 1, "synthetic-owned"),
                Err(Rejected::Protocol)
            );
        }
        assert_eq!(
            validate(
                &vec![b' '; protocol::MAX_STATUS_BYTES + 1],
                1,
                "synthetic-owned"
            ),
            Err(Rejected::Protocol)
        );
    }
}
