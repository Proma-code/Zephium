use super::*;
use zephium_core::work::model::*;

fn setup() -> (Hub, ProfileId) {
    let mut hub = Hub::in_memory().unwrap();
    let profile = ProfileId::from(1);
    hub.registry.insert(profile);
    (hub, profile)
}
fn model(name: &str) -> WorkModelEntry {
    WorkModelEntry {
        id: format!("openai/{name}"),
        model: WorkModelRef {
            provider: WorkModelProvider::OpenAi,
            wire: WorkModelWire::OpenAiResponses,
            model: name.into(),
        },
        display_name: name.into(),
        roles: vec![WorkModelRole::Page],
        recommended: false,
        context_window: 1000,
        max_output: 100,
        supports: WorkModelSupports {
            tools: true,
            vision: false,
            prompt_cache: false,
            reasoning: true,
            native_search: false,
        },
        price: None,
    }
}
fn codex(effort: CodexEffortChoice) -> Selection {
    Selection::Codex(CodexChoice {
        model: "original-model".into(),
        effort,
    })
}
fn load(hub: &mut Hub, profile: ProfileId) -> Snapshot {
    let Reply::Loaded(value) = hub.page_choice_call(profile, Call::Read) else {
        panic!("exact snapshot");
    };
    value
}
fn cas(hub: &mut Hub, expected: Snapshot, selection: Selection) -> Reply {
    hub.page_choice_call(
        expected.profile,
        Call::CompareAndSwap {
            expected: Box::new(expected),
            selection,
        },
    )
}
fn committed(reply: Reply) -> Snapshot {
    let Reply::Committed(value) = reply else {
        panic!("confirmed commit expected: {reply:?}");
    };
    value
}
fn raw(hub: &Hub, profile: ProfileId) -> [Option<String>; 4] {
    read(&hub.meta, profile).unwrap().1
}
#[test]
fn legacy_model_and_inactive_native_choice_survive_atomic_switches() {
    let (mut hub, profile) = setup();
    let keys = keys(profile);
    let legacy = serde_json::to_string_pretty(&model("legacy-model")).unwrap();
    assert!(hub.set_app_setting(&keys[0], &legacy).unwrap());
    assert!(hub
        .set_app_setting(&format!("work.models.{profile}.lead"), "lead-unchanged")
        .unwrap());
    let first = load(&mut hub, profile);
    assert_eq!(first.revision, 0);
    assert_eq!(first.backend, Backend::Model);
    let native = committed(cas(&mut hub, first, codex(CodexEffortChoice::Default)));
    assert_eq!(native.revision, 1);
    assert_eq!(native.backend, Backend::Codex);
    assert_eq!(hub.app_setting(&keys[0]), Some(legacy));
    let native_bytes = hub.app_setting(&keys[2]);
    let default = committed(cas(&mut hub, native.clone(), Selection::Model(None)));
    assert_eq!(default.revision, 2);
    assert_eq!(default.backend, Backend::Model);
    assert_eq!(default.codex, native.codex);
    assert_eq!(hub.app_setting(&keys[2]), native_bytes);
    assert_eq!(load(&mut hub, profile), default);
    assert_eq!(
        hub.app_setting(&format!("work.models.{profile}.lead")),
        Some("lead-unchanged".into())
    );
}
#[test]
fn explicit_none_is_not_default_and_old_explicit_effort_import_is_exact() {
    let (mut hub, profile) = setup();
    let keys = keys(profile);
    hub.set_app_setting(&keys[1], "codex").unwrap();
    hub.set_app_setting(&keys[2], r#"{"model":"original-model","effort":"none"}"#)
        .unwrap();
    let old = load(&mut hub, profile);
    assert_eq!(
        old.codex.as_ref().unwrap().effort,
        CodexEffortChoice::Explicit(CodexEffort::None)
    );
    let default = committed(cas(&mut hub, old, codex(CodexEffortChoice::Default)));
    assert_eq!(
        default.codex.as_ref().unwrap().effort,
        CodexEffortChoice::Default
    );
    assert!(hub
        .app_setting(&keys[2])
        .unwrap()
        .contains(r#""kind":"default""#));
}
#[test]
fn full_snapshot_cas_includes_inactive_values_and_native_profile() {
    let (mut hub, profile) = setup();
    let initial = load(&mut hub, profile);
    let native = committed(cas(
        &mut hub,
        initial.clone(),
        codex(CodexEffortChoice::Default),
    ));
    assert_eq!(
        cas(&mut hub, initial, Selection::Model(None)),
        Reply::Conflict(native.clone())
    );
    hub.set_app_setting(
        &keys(profile)[0],
        &serde_json::to_string(&model("changed-inactive")).unwrap(),
    )
    .unwrap();
    let current = load(&mut hub, profile);
    assert_eq!(current.revision, native.revision);
    assert_eq!(
        cas(&mut hub, native, Selection::Model(None)),
        Reply::Conflict(current.clone())
    );
    assert_eq!(
        hub.page_choice_call(
            ProfileId::from(2),
            Call::CompareAndSwap {
                expected: Box::new(current),
                selection: Selection::Model(None)
            }
        ),
        Reply::Failed(Fault::Invalid)
    );
    assert_eq!(
        hub.page_choice_call(ProfileId::from(2), Call::Read),
        Reply::Failed(Fault::NotRegistered)
    );
}
#[test]
fn failed_last_write_rolls_back_every_key_without_a_committed_receipt() {
    let (mut hub, profile) = setup();
    let initial = load(&mut hub, profile);
    let before = raw(&hub, profile);
    let sync: i64 = hub
        .meta
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    hub.meta.execute_batch("CREATE TRIGGER fail_revision BEFORE INSERT ON settings WHEN NEW.key LIKE 'codex.page.revision.%' BEGIN SELECT RAISE(FAIL,'synthetic'); END;").unwrap();
    assert_eq!(
        cas(&mut hub, initial.clone(), codex(CodexEffortChoice::Default)),
        Reply::Failed(Fault::OutcomeUnknown)
    );
    assert_eq!(raw(&hub, profile), before);
    assert_eq!(load(&mut hub, profile), initial);
    assert_eq!(
        hub.meta
            .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
            .unwrap(),
        sync
    );
}
#[test]
fn malformed_partial_or_future_preferences_never_fall_back_or_mutate() {
    for (index, value) in [
        (1, "future"),
        (1, "codex"),
        (2, r#"{"model":"original-model","effort":"ultra"}"#),
        (
            2,
            r#"{"model":"original-model","effort":{"kind":"future"}}"#,
        ),
        (3, "0"),
        (3, "01"),
        (3, "9223372036854775808"),
        (0, "not-json"),
    ] {
        let (mut hub, profile) = setup();
        let key = keys(profile)[index].clone();
        hub.set_app_setting(&key, value).unwrap();
        assert_eq!(
            hub.page_choice_call(profile, Call::Read),
            Reply::Failed(Fault::Corrupt)
        );
        assert_eq!(hub.app_setting(&key), Some(value.into()));
    }
}
#[test]
fn capacity_and_revision_exhaustion_leave_the_entire_previous_choice() {
    let (mut hub, profile) = setup();
    let initial = load(&mut hub, profile);
    for i in 0..settings::MAX_APP_SETTINGS {
        hub.set_app_setting(&format!("other-{i}"), "keep").unwrap();
    }
    assert_eq!(
        cas(&mut hub, initial.clone(), codex(CodexEffortChoice::Default)),
        Reply::Failed(Fault::Limit)
    );
    assert_eq!(load(&mut hub, profile), initial);
    let (mut hub, profile) = setup();
    hub.set_app_setting(&keys(profile)[3], &i64::MAX.to_string())
        .unwrap();
    let old = load(&mut hub, profile);
    assert_eq!(
        cas(&mut hub, old.clone(), Selection::Model(None)),
        Reply::Failed(Fault::RevisionExhausted)
    );
    assert_eq!(load(&mut hub, profile), old);
}
#[test]
fn confirmed_selection_reopens_from_disk_without_changing_other_profile() {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileId::from(1);
    let mut hub = Hub::open(dir.path().to_path_buf()).unwrap();
    hub.registry.insert(profile);
    let first = load(&mut hub, profile);
    let expected = committed(cas(
        &mut hub,
        first,
        codex(CodexEffortChoice::Explicit(CodexEffort::Max)),
    ));
    drop(hub);
    let mut hub = Hub::open(dir.path().to_path_buf()).unwrap();
    hub.registry.insert(profile);
    hub.registry.insert(ProfileId::from(2));
    assert_eq!(load(&mut hub, profile), expected);
    assert_eq!(
        load(&mut hub, ProfileId::from(2)).selection(),
        Some(Selection::Model(None))
    );
}
