use super::*;
use serde_json::json;

pub(crate) fn row() -> Value {
    json!({"id":"picker-id","model":"original-wire-model","displayName":"Original Model Name",
        "description":"Original description", "hidden":false,"isDefault":true,
        "inputModalities":["text","image"],"defaultReasoningEffort":"high",
        "supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High effort"},
            {"reasoningEffort":"max","description":"Maximum effort"},
            {"reasoningEffort":"ultra","description":"Original Ultra"}]})
}
#[test]
fn actual_ids_names_efforts_preserved_and_ultra_not_relabelled() {
    let mut builder = CatalogBuilder::new();
    builder
        .push_page(None, &json!({"data":[row()],"nextCursor":null}))
        .unwrap();
    let catalog = builder.finish().unwrap();
    let model = &catalog.models()[0];
    assert_eq!(
        (&*model.id, &*model.model, &*model.display_name),
        ("picker-id", "original-wire-model", "Original Model Name")
    );
    assert_eq!(
        model.efforts.iter().map(|e| e.effort).collect::<Vec<_>>(),
        [Effort::High, Effort::Max]
    );
    assert_eq!(model.default_effort, Some(Effort::High));
    let choice = catalog
        .select("picker-id", EffortSelection::Explicit(Effort::High))
        .unwrap();
    assert_eq!(choice.model(), "original-wire-model");
    assert_eq!(
        catalog.select(
            "original-wire-model",
            EffortSelection::Explicit(Effort::High)
        ),
        Err(CatalogFault::UnsupportedChoice)
    );
    assert_eq!(
        catalog.select("picker-id", EffortSelection::Explicit(Effort::Low)),
        Err(CatalogFault::UnsupportedChoice)
    );
    assert_eq!(serde_json::to_value(Effort::XHigh).unwrap(), "xhigh");
}

#[test]
fn unsupported_default_stays_absent_and_only_ultra_is_not_selectable() {
    let mut value = row();
    value["defaultReasoningEffort"] = json!("ultra");
    let mut builder = CatalogBuilder::new();
    builder
        .push_page(None, &json!({"data":[value],"nextCursor":null}))
        .unwrap();
    assert_eq!(builder.finish().unwrap().models()[0].default_effort, None);
    let mut value = row();
    value["defaultReasoningEffort"] = json!("ultra");
    value["supportedReasoningEfforts"] = json!([{"reasoningEffort":"ultra","description":"Ultra"}]);
    let mut builder = CatalogBuilder::new();
    builder
        .push_page(None, &json!({"data":[value],"nextCursor":null}))
        .unwrap();
    assert!(builder.finish().unwrap().models().is_empty());
}

#[test]
fn pagination_is_exact_bounded_atomic_and_no_partial_catalog() {
    let mut builder = CatalogBuilder::new();
    builder
        .push_page(None, &json!({"data":[row()],"nextCursor":"opaque-cursor"}))
        .unwrap();
    assert_eq!(builder.next_cursor(), Some("opaque-cursor"));
    assert_eq!(
        builder.push_page(None, &json!({"data":[],"nextCursor":null})),
        Err(CatalogFault::Cursor)
    );
    assert_eq!(
        builder.push_page(
            Some("opaque-cursor"),
            &json!({"data":[],"nextCursor":"opaque-cursor"})
        ),
        Err(CatalogFault::Cursor)
    );
    assert_eq!(
        builder.push_page(
            Some("opaque-cursor"),
            &json!({"data":[row()],"nextCursor":null})
        ),
        Err(CatalogFault::Duplicate)
    );
    assert_eq!(builder.next_cursor(), Some("opaque-cursor"));
    builder
        .push_page(Some("opaque-cursor"), &json!({"data":[],"nextCursor":null}))
        .unwrap();
    assert_eq!(builder.finish().unwrap().models().len(), 1);
    assert_eq!(
        CatalogBuilder::new().finish(),
        Err(CatalogFault::NotComplete)
    );
}

#[test]
fn malformed_shapes_hidden_rows_and_bounds() {
    for bad in [
        json!({}),
        json!({"data":[],"nextCursor":2}),
        json!({"data":null,"nextCursor":null}),
    ] {
        assert_eq!(
            CatalogBuilder::new().push_page(None, &bad),
            Err(CatalogFault::Shape)
        );
    }
    let mut value = row();
    value["hidden"] = json!(true);
    let mut builder = CatalogBuilder::new();
    builder
        .push_page(None, &json!({"data":[value],"nextCursor":null}))
        .unwrap();
    assert!(builder.finish().unwrap().models().is_empty());
    let mut value = row();
    value["displayName"] = json!("bad\nname");
    assert_eq!(
        CatalogBuilder::new().push_page(None, &json!({"data":[value],"nextCursor":null})),
        Err(CatalogFault::Shape)
    );
    assert_eq!(
        CatalogBuilder::new().push_page(
            None,
            &json!({"data":vec![row();MAX_MODELS+1],"nextCursor":null})
        ),
        Err(CatalogFault::Limit)
    );
}

fn catalog(value: Value) -> Catalog {
    let mut builder = CatalogBuilder::new();
    builder
        .push_page(None, &json!({"data":[value],"nextCursor":null}))
        .unwrap();
    builder.finish().unwrap()
}

#[test]
fn empty_presets_admit_original_default_without_fabricating_explicit_none() {
    let mut value = row();
    value["supportedReasoningEfforts"] = json!([]);
    value["defaultReasoningEffort"] = json!("none");
    let catalog = catalog(value);
    let model = &catalog.models()[0];
    assert!(model.efforts.is_empty());
    assert_eq!(model.default_effort_wire, "none");
    assert_eq!(model.default_effort, Some(Effort::None));
    let selected = catalog
        .select("picker-id", EffortSelection::Default)
        .unwrap();
    assert_eq!(selected.selection(), EffortSelection::Default);
    assert_eq!(selected.explicit_effort(), None);
    assert_eq!(selected.advertised_default(), Some(Effort::None));
    assert_eq!(selected.advertised_default_wire(), "none");
    assert_eq!(
        catalog.select("picker-id", EffortSelection::Explicit(Effort::None)),
        Err(CatalogFault::UnsupportedChoice)
    );
}

#[test]
fn default_outside_presets_stays_independent_and_never_clamps() {
    let mut value = row();
    value["supportedReasoningEfforts"] =
        json!([{"reasoningEffort":"low","description":"Original Low"}]);
    value["defaultReasoningEffort"] = json!("high");
    let catalog = catalog(value);
    let selected = catalog
        .select("picker-id", EffortSelection::Default)
        .unwrap();
    assert_eq!(selected.advertised_default(), Some(Effort::High));
    assert_eq!(selected.explicit_effort(), None);
    assert!(catalog
        .select("picker-id", EffortSelection::Explicit(Effort::Low))
        .is_ok());
    assert_eq!(
        catalog.select("picker-id", EffortSelection::Explicit(Effort::High)),
        Err(CatalogFault::UnsupportedChoice)
    );
}

#[test]
fn explicit_none_and_omitted_default_are_distinct_choices_and_dtos() {
    let mut value = row();
    value["supportedReasoningEfforts"] =
        json!([{"reasoningEffort":"none","description":"Original None"}]);
    value["defaultReasoningEffort"] = json!("none");
    let catalog = catalog(value);
    let default = catalog
        .select("picker-id", EffortSelection::Default)
        .unwrap();
    let explicit = catalog
        .select("picker-id", EffortSelection::Explicit(Effort::None))
        .unwrap();
    assert_ne!(default, explicit);
    assert_eq!(default.explicit_effort(), None);
    assert_eq!(explicit.explicit_effort(), Some(Effort::None));
    assert_eq!(
        serde_json::to_value(default.selection()).unwrap(),
        json!({"kind":"default"})
    );
    assert_eq!(
        serde_json::to_value(explicit.selection()).unwrap(),
        json!({"kind":"explicit","effort":"none"})
    );
    for bad in [
        json!("none"),
        json!({"kind":"explicit"}),
        json!({"kind":"explicit","effort":null}),
        json!({"kind":"default","effort":"none"}),
        json!({"kind":"default","extra":true}),
        json!({"kind":"explicit","effort":"ultra"}),
    ] {
        assert!(serde_json::from_value::<EffortSelection>(bad).is_err());
    }
}

#[test]
fn unqualified_default_keeps_exact_metadata_and_qualified_explicit_choice() {
    for wire in ["ultra", "persistent", "future-mode"] {
        let mut value = row();
        value["defaultReasoningEffort"] = json!(wire);
        let catalog = catalog(value);
        assert_eq!(catalog.models()[0].default_effort_wire, wire);
        assert_eq!(catalog.models()[0].default_effort, None);
        assert_eq!(
            catalog.select("picker-id", EffortSelection::Default),
            Err(CatalogFault::UnsupportedChoice)
        );
        let explicit = catalog
            .select("picker-id", EffortSelection::Explicit(Effort::High))
            .unwrap();
        assert_eq!(explicit.advertised_default_wire(), wire);
        assert_eq!(explicit.advertised_default(), None);
        assert_eq!(explicit.explicit_effort(), Some(Effort::High));
    }
}

#[test]
fn valid_unqualified_row_does_not_poison_other_models() {
    let mut unqualified = row();
    unqualified["id"] = json!("unqualified-picker");
    unqualified["supportedReasoningEfforts"] = json!([]);
    unqualified["defaultReasoningEffort"] = json!("future-mode");
    let mut builder = CatalogBuilder::new();
    builder
        .push_page(None, &json!({"data":[unqualified,row()],"nextCursor":null}))
        .unwrap();
    let catalog = builder.finish().unwrap();
    assert_eq!(catalog.models().len(), 1);
    assert!(catalog
        .select("picker-id", EffortSelection::Explicit(Effort::High))
        .is_ok());
}

#[test]
fn missing_or_null_catalog_default_is_not_an_original_none_value() {
    for missing in [false, true] {
        let mut value = row();
        if missing {
            value
                .as_object_mut()
                .unwrap()
                .remove("defaultReasoningEffort");
        } else {
            value["defaultReasoningEffort"] = Value::Null;
        }
        assert_eq!(
            CatalogBuilder::new().push_page(None, &json!({"data":[value],"nextCursor":null})),
            Err(CatalogFault::Shape)
        );
    }
}
