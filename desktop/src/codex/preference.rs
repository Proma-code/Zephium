//! Desired model/effort is local configuration, not runtime execution authority.
//! Healthy dormant metadata can validate an offered preference without consuming
//! custody. The next send still wakes and validates the original image, account,
//! catalog and policy. Active owners retain their existing queue/CAS lane.
use super::*;
use zephium_core::ports::store::Store as _;

pub(super) fn same_catalog(
    view: &CodexViewV1,
    selection: &page_choice::Selection,
    generation: Option<&str>,
) -> bool {
    !matches!(selection, page_choice::Selection::Codex(_))
        || generation.is_some_and(|value| view.catalog_generation.as_deref() == Some(value))
}

pub(super) fn active_current(service: &Service, call: &SelectionCall) -> bool {
    call.account_revision != 0
        && call.account_revision == service.account_revision.load(Ordering::Acquire)
        && same_catalog(
            &service.snapshot(),
            &call.selection,
            call.catalog_generation.as_deref(),
        )
}

fn offered(view: &CodexViewV1, choice: &page_choice::CodexChoice) -> bool {
    view.state == CodexStateV1::Connected
        && view.models.iter().any(|model| {
            model.id == choice.model
                && match choice.effort {
                    CodexEffortSelectionV1::Default => model.default_effort.is_some(),
                    CodexEffortSelectionV1::Explicit(effort) => model.efforts.contains(&effort),
                }
        })
}

/// Called synchronously under the same registry lock as owner creation and
/// profile retirement. It must never spawn, consume custody, or perform I/O
/// other than the bounded local Store transaction.
pub(super) fn commit_settled(
    owner: Option<&Arc<Service>>,
    epoch: u64,
    expected: &page_choice::Snapshot,
    selection: &page_choice::Selection,
    generation: Option<&str>,
) -> Result<page_choice::Reply, CodexFaultV1> {
    commit_settled_with_store(owner, epoch, expected, selection, generation, || {
        APP_STORE
            .get()
            .map(Arc::as_ref)
            .ok_or(CodexFaultV1::ProfileStorage)
    })
}
fn commit_settled_with_store<'a>(
    owner: Option<&Arc<Service>>,
    epoch: u64,
    expected: &page_choice::Snapshot,
    selection: &page_choice::Selection,
    generation: Option<&str>,
    store: impl FnOnce() -> Result<&'a zephium_store::SqliteStore, CodexFaultV1>,
) -> Result<page_choice::Reply, CodexFaultV1> {
    if let page_choice::Selection::Codex(choice) = selection {
        let owner = owner.ok_or(CodexFaultV1::Unavailable)?;
        if owner.profile != expected.profile
            || owner.epoch != epoch
            || owner.stop.load(Ordering::Acquire)
            || !owner.fully_settled()
            || !dormant::healthy(owner)
            || !owner.removal.lock().is_ok_and(|value| value.is_none())
        {
            return Err(CodexFaultV1::ScopeChanged);
        }
        let view = owner.snapshot();
        if !same_catalog(&view, selection, generation) {
            return Err(CodexFaultV1::ScopeChanged);
        }
        if !offered(&view, choice) {
            return Err(CodexFaultV1::Invalid);
        }
    }
    let store = store()?;
    let receipt = store.page_choice_call(
        expected.profile,
        page_choice::Call::CompareAndSwap {
            expected: Box::new(expected.clone()),
            selection: selection.clone(),
        },
    );
    match owner {
        Some(owner) => confirm(
            owner,
            expected.updated(selection.clone()).as_ref(),
            receipt,
            None,
        ),
        None => Ok(receipt),
    }
}

pub(super) fn confirm(
    service: &Service,
    wanted: Option<&page_choice::Snapshot>,
    receipt: page_choice::Reply,
    account_revision: Option<u64>,
) -> Result<page_choice::Reply, CodexFaultV1> {
    match &receipt {
        page_choice::Reply::Committed(value)
            if Some(value) == wanted && observe_selection(service, value, account_revision) =>
        {
            Ok(receipt)
        }
        page_choice::Reply::Conflict(value)
            if value.profile == service.profile
                && value.valid()
                && observe_selection(service, value, account_revision) =>
        {
            Ok(receipt)
        }
        _ => {
            selection_uncertain(service);
            Err(CodexFaultV1::OutcomeUnknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::owned_service;
    use super::*;

    fn choice(effort: CodexEffortSelectionV1) -> page_choice::CodexChoice {
        page_choice::CodexChoice {
            model: "original-model".into(),
            effort,
        }
    }
    fn snapshot(profile: ProfileId) -> page_choice::Snapshot {
        page_choice::Snapshot {
            profile,
            revision: 0,
            backend: page_choice::Backend::Model,
            model: None,
            codex: None,
        }
    }
    fn metadata(service: &Service) {
        let mut view = service.view.lock().unwrap();
        view.state = CodexStateV1::Connected;
        view.catalog_generation = Some("original-catalog".into());
        view.models = vec![CodexModelV1 {
            id: "original-model".into(),
            name: "Original".into(),
            efforts: vec![CodexEffortV1::High],
            default_effort: Some(CodexEffortV1::High),
        }];
    }
    #[cfg(all(windows, debug_assertions))]
    #[test]
    fn healthy_dormant_preference_commits_real_store_without_runtime_io() {
        use zephium_core::{
            profiles::ProfileKind,
            session::{PersistedProfile, SessionState},
        };
        let helper = std::env::var_os("ZEPHIUM_CODEX_FIXTURE_EXE")
            .map(std::path::PathBuf::from)
            .expect("Windows cohort must build and identify the fixed synthetic fixture helper");
        let (root, receipt, original_catalog) =
            zephium_codex_broker::fixture_dormant_preference(&helper).unwrap();
        let (mut service, receiver) = owned_service();
        service.profile = ProfileId::from(u128::from_be_bytes(receipt.profile().bytes()));
        *service.view.lock().unwrap() = CodexViewV1::unavailable(service.profile.to_string());
        state(&service, AccountState::ChatGpt);
        publish_catalog_metadata(&service, &original_catalog);
        drop(service.queue.try_park(|| true).unwrap());
        drop(receiver);
        *service.dormant.lock().unwrap() = Some(dormant::Custody::Healthy {
            receipt,
            quota: None,
        });
        service.done.store(true, Ordering::Release);
        dormant::publish_returned(&service, false);
        // Store pins an existing application-data directory; it does not create it.
        let store_dir = root.path().join("selection-store");
        std::fs::create_dir(&store_dir).unwrap();
        let store = zephium_store::SqliteStore::open(&store_dir).unwrap();
        store.save_session(SessionState {
            profiles: vec![PersistedProfile {
                id: service.profile,
                name: "Synthetic preference".into(),
                kind: ProfileKind::Default,
            }],
            spaces: vec![],
            items: vec![],
            active_space: None,
            active_item: None,
            splits: None,
            recently_closed: vec![],
        });
        let page_choice::Reply::Loaded(before) =
            store.page_choice_call(service.profile, page_choice::Call::Read)
        else {
            panic!("registered local profile")
        };
        assert!(observe_selection(&service, &before, Some(0)));
        let view = service.snapshot();
        let generation = view.catalog_generation.clone().unwrap();
        let model = view.models.first().unwrap();
        let desired = page_choice::Selection::Codex(page_choice::CodexChoice {
            model: model.id.clone(),
            effort: CodexEffortSelectionV1::Explicit(*model.efforts.first().unwrap()),
        });
        let expected = before.updated(desired.clone()).unwrap();
        let owner = Arc::new(service);
        let owners = Mutex::new(HashMap::from([(owner.profile, owner.clone())]));
        let activity = zephium_codex_broker::native_fixture_activity();
        let start = Instant::now();
        let route = admission::route_selection(
            &owners,
            owner.profile,
            owner.epoch,
            || Some(owner.epoch),
            |owner| {
                assert!(owner.fully_settled());
                admission::OwnerState::Settled
            },
            |owner| {
                commit_settled_with_store(owner, 1, &before, &desired, Some(&generation), || {
                    Ok(&store)
                })
            },
        )
        .unwrap();
        let admission::SelectionRoute::Committed(result) = route else {
            panic!("must not enqueue or wake")
        };
        assert_eq!(result, Ok(page_choice::Reply::Committed(expected.clone())));
        eprintln!(
            "healthy dormant preference exact registry/custody/catalog/SQLite CAS: {:?}",
            start.elapsed()
        );
        assert_eq!(
            store.page_choice_call(owner.profile, page_choice::Call::Read),
            page_choice::Reply::Loaded(expected)
        );
        assert_eq!(
            zephium_codex_broker::native_fixture_activity(),
            activity,
            "discovery, image preparation, process spawn and network must remain unchanged"
        );
        assert!(dormant::healthy(&owner));
        assert_eq!(
            owner.snapshot().catalog_generation.as_deref(),
            Some(generation.as_str())
        );
        assert!(owner.queue.is_parked());
        assert_eq!(owner.account_revision.load(Ordering::Acquire), 0);
        assert!(owner.snapshot().text_only);
        assert!(matches!(owner.queue.reserve(), Err(admission::Error::Busy)));
        assert_eq!(
            commit_settled_with_store(
                Some(&owner),
                1,
                &before,
                &desired,
                Some("stale-catalog"),
                || panic!("stale metadata must not reach Store")
            ),
            Err(CodexFaultV1::ScopeChanged)
        );
        assert_eq!(
            commit_settled_with_store(
                Some(&owner),
                2,
                &before,
                &desired,
                Some(&generation),
                || panic!("stale profile must not reach Store")
            ),
            Err(CodexFaultV1::ScopeChanged)
        );
        owner.request_stop();
        assert_eq!(
            commit_settled_with_store(
                Some(&owner),
                1,
                &before,
                &desired,
                Some(&generation),
                || panic!("retired owner must not reach Store")
            ),
            Err(CodexFaultV1::ScopeChanged)
        );
    }

    #[test]
    fn observed_metadata_preserves_exact_default_and_explicit_membership() {
        let (service, _receiver) = owned_service();
        metadata(&service);
        let mut view = service.snapshot();
        assert!(offered(&view, &choice(CodexEffortSelectionV1::Default)));
        assert!(offered(
            &view,
            &choice(CodexEffortSelectionV1::Explicit(CodexEffortV1::High))
        ));
        assert!(!offered(
            &view,
            &choice(CodexEffortSelectionV1::Explicit(CodexEffortV1::Max))
        ));
        view.models[0].default_effort = None;
        assert!(!offered(&view, &choice(CodexEffortSelectionV1::Default)));
        view.state = CodexStateV1::SignedOut;
        assert!(!offered(
            &view,
            &choice(CodexEffortSelectionV1::Explicit(CodexEffortV1::High))
        ));
    }
    #[test]
    fn catalog_fence_is_not_preference_revision_or_runtime_authority() {
        let (service, _receiver) = owned_service();
        metadata(&service);
        let desired = page_choice::Selection::Codex(choice(CodexEffortSelectionV1::Default));
        assert!(!same_catalog(&service.snapshot(), &desired, None));
        assert!(!same_catalog(
            &service.snapshot(),
            &desired,
            Some("previous-account")
        ));
        let prior = snapshot(service.profile);
        let next = prior.updated(desired.clone()).unwrap();
        service.done.store(true, Ordering::Release);
        service.account_revision.store(0, Ordering::Release);
        service.view.lock().unwrap().runtime = CodexRuntimeV1::Dormant;
        assert!(confirm(
            &service,
            Some(&next),
            page_choice::Reply::Committed(next.clone()),
            None
        )
        .is_ok());
        assert!(same_catalog(
            &service.snapshot(),
            &desired,
            Some("original-catalog")
        ));
        assert_eq!(service.account_revision.load(Ordering::Acquire), 0);
        assert!(service.snapshot().text_only);
        assert!(matches!(
            service.queue.reserve(),
            Err(admission::Error::Busy)
        ));
        state(&service, AccountState::Unknown);
        assert!(!same_catalog(
            &service.snapshot(),
            &desired,
            Some("original-catalog")
        ));
    }
    #[test]
    fn account_change_or_unknown_store_receipt_never_publishes_requested_choice() {
        for receipt in [
            page_choice::Reply::Failed(page_choice::Fault::OutcomeUnknown),
            page_choice::Reply::Loaded(snapshot(ProfileId::from(1_u128))),
        ] {
            let (service, _receiver) = owned_service();
            let next = snapshot(service.profile)
                .updated(page_choice::Selection::Codex(choice(
                    CodexEffortSelectionV1::Default,
                )))
                .unwrap();
            assert_eq!(
                confirm(&service, Some(&next), receipt, Some(1)),
                Err(CodexFaultV1::OutcomeUnknown)
            );
            assert!(service.snapshot().selection.is_none());
            assert_eq!(service.selection_revision.load(Ordering::Acquire), u64::MAX);
        }
        let (service, _receiver) = owned_service();
        let next = snapshot(service.profile)
            .updated(page_choice::Selection::Codex(choice(
                CodexEffortSelectionV1::Default,
            )))
            .unwrap();
        service.account_revision.store(2, Ordering::Release);
        assert_eq!(
            confirm(
                &service,
                Some(&next),
                page_choice::Reply::Committed(next.clone()),
                Some(1)
            ),
            Err(CodexFaultV1::OutcomeUnknown)
        );
        assert!(service.snapshot().selection.is_none());
    }
    #[test]
    fn stale_selection_receipt_cannot_replace_latest_confirmed_desire() {
        let (service, _receiver) = owned_service();
        metadata(&service);
        service.done.store(true, Ordering::Release);
        service.account_revision.store(0, Ordering::Release);
        service.view.lock().unwrap().runtime = CodexRuntimeV1::Dormant;
        let first = snapshot(service.profile)
            .updated(page_choice::Selection::Codex(choice(
                CodexEffortSelectionV1::Default,
            )))
            .unwrap();
        let latest = first
            .updated(page_choice::Selection::Codex(choice(
                CodexEffortSelectionV1::Explicit(CodexEffortV1::High),
            )))
            .unwrap();
        assert!(confirm(
            &service,
            Some(&latest),
            page_choice::Reply::Committed(latest.clone()),
            None
        )
        .is_ok());
        assert_eq!(
            confirm(
                &service,
                Some(&first),
                page_choice::Reply::Committed(first.clone()),
                None
            ),
            Err(CodexFaultV1::OutcomeUnknown)
        );
        assert_eq!(service.snapshot().selection, latest.codex.map(view_choice));
        assert_eq!(service.account_revision.load(Ordering::Acquire), 0);
    }

    #[test]
    fn active_turn_busy_and_changed_account_reject_before_preference_commit() {
        let (service, _receiver) = owned_service();
        metadata(&service);
        service.queue.set_ready(true);
        let turn = service.queue.reserve().unwrap();
        let (result, _receive) = tokio::sync::oneshot::channel();
        let call = SelectionCall {
            expected: snapshot(service.profile),
            selection: page_choice::Selection::Codex(choice(CodexEffortSelectionV1::Default)),
            deadline: Instant::now() + Duration::from_secs(8),
            account_revision: 1,
            catalog_generation: Some("original-catalog".into()),
            result,
        };
        assert!(active_current(&service, &call));
        assert_eq!(
            service.enqueue(Command::Select(Box::new(call))),
            Err(CodexFaultV1::Busy)
        );
        assert_eq!(service.account_revision.load(Ordering::Acquire), 1);
        assert!(service.snapshot().selection.is_none());
        drop(turn);
        let (result, _receive) = tokio::sync::oneshot::channel();
        let call = SelectionCall {
            expected: snapshot(service.profile),
            selection: page_choice::Selection::Codex(choice(CodexEffortSelectionV1::Default)),
            deadline: Instant::now() + Duration::from_secs(8),
            account_revision: 1,
            catalog_generation: Some("original-catalog".into()),
            result,
        };
        service.account_revision.store(2, Ordering::Release);
        assert!(!active_current(&service, &call));
        service.account_revision.store(1, Ordering::Release);
        service.view.lock().unwrap().catalog_generation =
            Some("replacement-account-same-models".into());
        assert!(!active_current(&service, &call));
    }

    #[test]
    fn absent_or_merely_finished_owner_cannot_edit_codex_from_cached_rows() {
        let (service, _receiver) = owned_service();
        metadata(&service);
        let expected = snapshot(service.profile);
        let desired = page_choice::Selection::Codex(choice(CodexEffortSelectionV1::Default));
        assert_eq!(
            commit_settled(None, 1, &expected, &desired, Some("original-catalog")),
            Err(CodexFaultV1::Unavailable)
        );
        service.done.store(true, Ordering::Release);
        service.view.lock().unwrap().runtime = CodexRuntimeV1::Dormant;
        let owner = Arc::new(service);
        // No genuine healthy receipt: neither done nor displayed Dormant can mint it.
        assert_eq!(
            commit_settled(
                Some(&owner),
                1,
                &expected,
                &desired,
                Some("original-catalog")
            ),
            Err(CodexFaultV1::ScopeChanged)
        );
        assert_eq!(
            commit_settled(
                Some(&owner),
                2,
                &expected,
                &desired,
                Some("original-catalog")
            ),
            Err(CodexFaultV1::ScopeChanged)
        );
        owner.request_stop();
        assert!(owner.snapshot().catalog_generation.is_none());
        assert_eq!(
            commit_settled(
                Some(&owner),
                1,
                &expected,
                &desired,
                Some("original-catalog")
            ),
            Err(CodexFaultV1::ScopeChanged)
        );
    }
}
