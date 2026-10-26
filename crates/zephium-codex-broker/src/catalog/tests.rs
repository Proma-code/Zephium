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
    let choice = catalog.select("picker-id", Effort::High).unwrap();
    assert_eq!(choice.model(), "original-wire-model");
    assert_eq!(
        catalog.select("original-wire-model", Effort::High),
        Err(CatalogFault::UnsupportedChoice)
    );
    assert_eq!(
        catalog.select("picker-id", Effort::Low),
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
