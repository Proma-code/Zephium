//! Effective readback for the exact native selection; omission is not `none`.
use crate::catalog::{EffortSelection, ModelChoice};
use serde_json::Value;

pub(crate) fn readback(choice: &ModelChoice, value: Option<&Value>) -> bool {
    match (choice.selection(), value) {
        (EffortSelection::Default, Some(Value::Null)) => true,
        (EffortSelection::Default, Some(Value::String(actual))) => choice
            .advertised_default()
            .is_some_and(|expected| actual == expected.as_str()),
        (EffortSelection::Explicit(expected), Some(Value::String(actual))) => {
            actual == expected.as_str()
        }
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::catalog::{CatalogBuilder, Effort};
    use serde_json::json;

    pub(crate) fn choice(selection: EffortSelection) -> ModelChoice {
        let mut catalog = CatalogBuilder::new();
        catalog.push_page(None, &json!({"data":[{"id":"picker","model":"original-model","displayName":"Original","description":"fixture","hidden":false,"isDefault":true,"inputModalities":["text"],"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"},{"reasoningEffort":"none","description":"None"}]}],"nextCursor":null})).unwrap();
        catalog
            .finish()
            .unwrap()
            .select("picker", selection)
            .unwrap()
    }
    #[test]
    fn default_requires_present_null_or_exact_qualified_default() {
        let selected = choice(EffortSelection::Default);
        assert!(readback(&selected, Some(&Value::Null)));
        assert!(readback(&selected, Some(&json!("high"))));
        for value in [
            None,
            Some(json!("none")),
            Some(json!("max")),
            Some(json!(false)),
            Some(json!(0)),
            Some(json!({})),
        ] {
            assert!(!readback(&selected, value.as_ref()));
        }
    }
    #[test]
    fn explicit_none_is_not_default_null_or_missing() {
        let selected = choice(EffortSelection::Explicit(Effort::None));
        assert!(readback(&selected, Some(&json!("none"))));
        for value in [
            None,
            Some(Value::Null),
            Some(json!("high")),
            Some(json!("default")),
        ] {
            assert!(!readback(&selected, value.as_ref()));
        }
    }
}
