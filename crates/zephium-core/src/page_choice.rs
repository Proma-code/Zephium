//! Atomic Page source/model selection. Stored values are bounded preferences,
//! not account authority or permission to execute a model.
use crate::{ids::ProfileId, work::model::WorkModelEntry};
use serde::{Deserialize, Serialize};

pub const MAX_VALUE_BYTES: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Model,
    Codex,
}
#[cfg_attr(feature = "ipc-types", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}
#[cfg_attr(feature = "ipc-types", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "effort",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CodexEffortChoice {
    Default,
    Explicit(CodexEffort),
}
#[cfg_attr(feature = "ipc-types", derive(specta::Type))]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexChoice {
    pub model: String,
    pub effort: CodexEffortChoice,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selection {
    Model(Option<WorkModelEntry>),
    Codex(CodexChoice),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub profile: ProfileId,
    /// Zero means the exact legacy cohort has not yet had a canonical commit.
    pub revision: u64,
    pub backend: Backend,
    /// Inactive choices are retained, never mistaken for the selected source.
    pub model: Option<WorkModelEntry>,
    pub codex: Option<CodexChoice>,
}
impl Snapshot {
    pub fn valid(&self) -> bool {
        self.revision <= i64::MAX as u64
            && self.model.as_ref().is_none_or(valid_model)
            && self.codex.as_ref().is_none_or(CodexChoice::valid)
            && (self.backend != Backend::Codex || self.codex.is_some())
    }
    pub fn selection(&self) -> Option<Selection> {
        match self.backend {
            Backend::Model => Some(Selection::Model(self.model.clone())),
            Backend::Codex => self.codex.clone().map(Selection::Codex),
        }
    }
    /// Pure expected-value construction, not a durable commit receipt.
    pub fn updated(&self, selection: Selection) -> Option<Self> {
        if !self.valid() || !selection.valid() {
            return None;
        }
        let revision = self
            .revision
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)?;
        let mut next = self.clone();
        next.revision = revision;
        match selection {
            Selection::Model(choice) => {
                next.backend = Backend::Model;
                next.model = choice;
            }
            Selection::Codex(choice) => {
                next.backend = Backend::Codex;
                next.codex = Some(choice);
            }
        }
        Some(next)
    }
}
impl CodexChoice {
    pub fn valid(&self) -> bool {
        bounded(&self.model, 256)
    }
}
impl Selection {
    pub fn valid(&self) -> bool {
        match self {
            Self::Model(choice) => choice.as_ref().is_none_or(valid_model),
            Self::Codex(choice) => choice.valid(),
        }
    }
}
fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn valid_model(entry: &WorkModelEntry) -> bool {
    bounded(&entry.id, 256)
        && bounded(&entry.model.model, 256)
        && bounded(&entry.display_name, 1024)
        && entry.roles.len() <= 4
        && entry
            .roles
            .iter()
            .enumerate()
            .all(|(i, role)| !entry.roles[..i].contains(role))
        && serde_json::to_vec(entry).is_ok_and(|bytes| bytes.len() <= MAX_VALUE_BYTES)
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Call {
    Read,
    CompareAndSwap {
        expected: Box<Snapshot>,
        selection: Selection,
    },
}
impl Call {
    pub fn valid(&self, profile: ProfileId) -> bool {
        match self {
            Self::Read => true,
            Self::CompareAndSwap {
                expected,
                selection,
            } => expected.profile == profile && expected.valid() && selection.valid(),
        }
    }
    pub fn mutates(&self) -> bool {
        matches!(self, Self::CompareAndSwap { .. })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    Invalid,
    NotRegistered,
    Unavailable,
    Corrupt,
    Limit,
    RevisionExhausted,
    /// A queued mutation or commit did not return a confirmed durable receipt.
    /// Reconcile; never publish the requested choice optimistically or retry it.
    OutcomeUnknown,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reply {
    Loaded(Snapshot),
    Committed(Snapshot),
    Conflict(Snapshot),
    Failed(Fault),
}
/// Existing key cohort plus its canonical monotonic revision. No other role's
/// choice or reasoning setting participates in this transaction.
pub fn keys(profile: ProfileId) -> [String; 4] {
    [
        format!("work.models.{profile}.page"),
        format!("codex.page.backend.v1.{profile}"),
        format!("codex.page.selection.v1.{profile}"),
        format!("codex.page.revision.v1.{profile}"),
    ]
}
/// Generic asynchronous settings writes must not bypass the atomic Page port.
pub fn reserved_key(key: &str) -> bool {
    key.strip_prefix("work.models.")
        .and_then(|v| v.strip_suffix(".page"))
        .is_some_and(|v| ProfileId::parse(v).is_some())
        || [
            "codex.page.backend.v1.",
            "codex.page.selection.v1.",
            "codex.page.revision.v1.",
        ]
        .iter()
        .any(|prefix| {
            key.strip_prefix(prefix)
                .is_some_and(|v| ProfileId::parse(v).is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_and_explicit_none_are_distinct_exact_wire_choices() {
        assert_eq!(
            serde_json::to_string(&CodexEffortChoice::Default).unwrap(),
            r#"{"kind":"default"}"#
        );
        assert_eq!(
            serde_json::to_string(&CodexEffortChoice::Explicit(CodexEffort::None)).unwrap(),
            r#"{"kind":"explicit","effort":"none"}"#
        );
        for value in [
            r#"{"kind":"default","effort":"high"}"#,
            r#"{"kind":"explicit","effort":"ultra"}"#,
            r#"{"kind":"future"}"#,
        ] {
            assert!(serde_json::from_str::<CodexEffortChoice>(value).is_err());
        }
    }
    #[test]
    fn pure_update_preserves_inactive_choice_and_does_not_cross_profile_or_overflow() {
        let profile = ProfileId::from(1);
        let native = CodexChoice {
            model: "original-model".into(),
            effort: CodexEffortChoice::Default,
        };
        let old = Snapshot {
            profile,
            revision: 4,
            backend: Backend::Codex,
            model: None,
            codex: Some(native.clone()),
        };
        let updated = old.updated(Selection::Model(None)).unwrap();
        assert_eq!(updated.revision, 5);
        assert_eq!(updated.codex, Some(native));
        assert_eq!(updated.selection(), Some(Selection::Model(None)));
        assert!(!Call::CompareAndSwap {
            expected: Box::new(old.clone()),
            selection: Selection::Model(None)
        }
        .valid(ProfileId::from(2)));
        let exhausted = Snapshot {
            revision: i64::MAX as u64,
            ..old
        };
        assert!(exhausted.updated(Selection::Model(None)).is_none());
    }
    #[test]
    fn only_exact_page_key_cohort_is_reserved_from_generic_writers() {
        let profile = ProfileId::from(1);
        for key in keys(profile) {
            assert!(reserved_key(&key));
        }
        for key in [
            format!("work.models.{profile}.lead"),
            format!("work.models.{profile}.lead.effort"),
            format!("work.models.{profile}.light"),
            "codex.page.backend.v1.not-a-profile".into(),
            "page".into(),
        ] {
            assert!(!reserved_key(&key));
        }
    }
}
