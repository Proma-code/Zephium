//! Pre-say demand/lifecycle fixtures. No runtime, account or network.
use super::*;
use std::sync::atomic::AtomicUsize;

struct DemandFixture {
    browser: Arc<BrowserFixture>,
    active: AtomicBool,
    prepares: AtomicUsize,
    observations: AtomicUsize,
    holding: AtomicBool,
    releases: AtomicUsize,
    wake: Mutex<Option<oneshot::Receiver<()>>>,
    reserve_wait: Mutex<Option<oneshot::Receiver<()>>>,
    reserve_entered: AtomicBool,
    entry_wait: Mutex<Option<oneshot::Receiver<()>>>,
    entry_entered: AtomicBool,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    deadline: Mutex<Option<Instant>>,
    switch_to_model: AtomicBool,
    fail: Mutex<Option<PageAssistNativeFaultV1>>,
}
struct DemandModels(Arc<DemandFixture>);
struct Demand(Arc<DemandFixture>);
impl Drop for Demand {
    fn drop(&mut self) {
        self.0.holding.store(false, Ordering::Release);
        self.0.releases.fetch_add(1, Ordering::AcqRel);
    }
}
impl NativeConversationDemand for Demand {
    fn reserve_browser(
        self: Box<Self>,
        expected: NativeConversationChoice,
        draft: NativeConversationDraft,
        manifest: NativeBrowserManifest,
    ) -> BoxFuture<'static, Result<Box<dyn NativeConversationReservation>, PageBackendError>> {
        Box::pin(async move {
            self.0.reserve_entered.store(true, Ordering::Release);
            let wait = self.0.reserve_wait.lock().unwrap().take();
            if let Some(wait) = wait {
                let _ = wait.await;
            }
            assert!(!self
                .0
                .cancel
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .load(Ordering::Acquire));
            assert!(Instant::now() < self.0.deadline.lock().unwrap().unwrap());
            BrowserModels(self.0.browser.clone())
                .reserve_native_browser(&profile(), expected, draft, manifest)
                .await
        })
    }
}
impl ModelSource for DemandModels {
    fn prepare_reply<'a>(
        &'a self,
        requested: &'a ProfileId,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
    ) -> BoxFuture<'a, Result<Option<Box<dyn NativeConversationDemand>>, PageBackendError>> {
        Box::pin(async move {
            assert_eq!(*requested, profile());
            self.0.prepares.fetch_add(1, Ordering::AcqRel);
            assert!(!self.0.holding.swap(true, Ordering::AcqRel));
            let hold = Demand(self.0.clone());
            *self.0.cancel.lock().unwrap() = Some(cancel);
            *self.0.deadline.lock().unwrap() = Some(deadline);
            let wake = self.0.wake.lock().unwrap().take();
            if let Some(wake) = wake {
                let _ = wake.await;
            }
            if let Some(fault) = self.0.fail.lock().unwrap().take() {
                return Err(PageBackendError::NativeConversation(fault));
            }
            self.0.active.store(true, Ordering::Release);
            Ok(Some(Box::new(hold) as Box<dyn NativeConversationDemand>))
        })
    }
    fn dormant<'a>(
        &'a self,
        requested: &'a ProfileId,
    ) -> BoxFuture<'a, Option<NativeDormantChoice>> {
        Box::pin(async move {
            assert_eq!(*requested, profile());
            self.0.observations.fetch_add(1, Ordering::AcqRel);
            (!self.0.active.load(Ordering::Acquire)).then(|| NativeDormantChoice {
                id: browser_choice().id,
                name: browser_choice().name,
                source: PageAssistModelSourceV1::Codex,
                effort: None,
            })
        })
    }
    fn entry<'a>(&'a self, requested: &'a ProfileId) -> BoxFuture<'a, Option<PageBackendChoice>> {
        Box::pin(async move {
            assert_eq!(*requested, profile());
            if !self.0.active.load(Ordering::Acquire) {
                return None;
            }
            let wait = self.0.entry_wait.lock().unwrap().take();
            if let Some(wait) = wait {
                self.0.entry_entered.store(true, Ordering::Release);
                let _ = wait.await;
            }
            Some(if self.0.switch_to_model.load(Ordering::Acquire) {
                PageBackendChoice::Model(ModelChoice {
                    entry: entry(true),
                    effort: None,
                })
            } else {
                PageBackendChoice::NativeConversation(browser_choice())
            })
        })
    }
    fn resolve<'a>(
        &'a self,
        _: &'a ProfileId,
        _: WorkCallSource,
    ) -> BoxFuture<'a, Result<ResolvedPageBackend, PageBackendError>> {
        panic!("dormant or source switch cannot resolve a model")
    }
    fn reserve_native_browser<'a>(
        &'a self,
        _: &'a ProfileId,
        _: NativeConversationChoice,
        _: NativeConversationDraft,
        _: NativeBrowserManifest,
    ) -> BoxFuture<'a, Result<Box<dyn NativeConversationReservation>, PageBackendError>> {
        panic!("public ask must consume its retained demand, never reserve again")
    }
}
fn demand_harness() -> (Harness, Arc<DemandFixture>, oneshot::Sender<()>) {
    let (mut h, browser) = browser_harness(vec![], "Original reply");
    let (wake, waiting) = oneshot::channel();
    let fixture = Arc::new(DemandFixture {
        browser,
        active: AtomicBool::new(false),
        prepares: AtomicUsize::new(0),
        observations: AtomicUsize::new(0),
        holding: AtomicBool::new(false),
        releases: AtomicUsize::new(0),
        wake: Mutex::new(Some(waiting)),
        reserve_wait: Mutex::new(None),
        reserve_entered: AtomicBool::new(false),
        entry_wait: Mutex::new(None),
        entry_entered: AtomicBool::new(false),
        cancel: Mutex::new(None),
        deadline: Mutex::new(None),
        switch_to_model: AtomicBool::new(false),
        fail: Mutex::new(None),
    });
    let mut ports = h.service.ports().clone();
    ports.models = Arc::new(DemandModels(fixture.clone()));
    h.service = PageAssist::new(ports);
    (h, fixture, wake)
}
fn request(h: &Harness) -> PageAssistAskV1 {
    native_ask(
        h.service.current(SPACE, profile()),
        "  unchanged draft\r\n  ",
    )
}
fn assert_unsent(h: &Harness, fixture: &DemandFixture) {
    assert!(h.service.state().running.is_empty());
    assert!(h.service.state().preparing.is_empty());
    assert!(h
        .service
        .state()
        .live
        .values()
        .all(|live| live.conversation.messages().is_empty()));
    assert!(h.sink.0.lock().unwrap().is_empty());
    assert!(!fixture.browser.committed.load(Ordering::Acquire));
    assert!(!fixture.browser.provider_started.load(Ordering::Acquire));
    assert!(fixture.browser.drafts.lock().unwrap().is_empty());
    assert!(h.client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dormant_local_draft_status_pick_and_selection_never_wake_runtime() {
    let (h, fixture, _wake) = demand_harness();
    h.page.lock().selection = Some("selected passage".into());
    h.page.lock().picks.push(Pick {
        pick: 4,
        label: "Card".into(),
        text: "local content".into(),
        selector: None,
    });
    let status = h.service.status(SPACE, Some(TAB)).await;
    assert_eq!(status.availability, PageAssistAvailabilityV1::Dormant);
    assert_eq!(status.model.unwrap().capabilities, None);
    assert!(status.can_pick);
    assert!(status.page.is_some());
    assert!(!status.suggestions.is_empty());
    assert_eq!(status.selection.as_deref(), Some("selected passage"));
    assert!(h.service.pick(SPACE, TAB).await.pick.is_some());
    assert_eq!(fixture.prepares.load(Ordering::Acquire), 0);
    assert_eq!(h.page.lock().reads, 1);
    assert_eq!(
        h.service.availability(h.page.facts(TAB).as_ref()).await,
        PageAssistAvailabilityV1::NoModel
    );
    assert_eq!(
        policy::refusal(PageAssistAvailabilityV1::Dormant),
        Some(PageAssistFaultV1::Unavailable)
    );
    assert_unsent(&h, &fixture);
}

#[tokio::test]
async fn dormant_draft_controls_preserve_private_ai_and_profile_boundaries() {
    for case in 0..4 {
        let (h, fixture, _wake) = demand_harness();
        match case {
            0 => h.page.lock().facts.get_mut(TAB).unwrap().private = true,
            1 => h.ai.0.store(false, Ordering::Release),
            2 => h.page.lock().facts.get_mut(TAB).unwrap().profile = ProfileId::from(9),
            _ => h.page.lock().facts.get_mut(TAB).unwrap().space = Some("other".into()),
        }
        let status = h.service.status(SPACE, Some(TAB)).await;
        assert!(!status.can_pick);
        assert!(status.page.is_none());
        assert!(status.selection.is_none());
        assert!(h.service.pick(SPACE, TAB).await.pick.is_none());
        assert_eq!(h.page.lock().reads, 0);
        assert_eq!(fixture.prepares.load(Ordering::Acquire), 0);
        assert!(h.client.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn deliberate_dormant_ask_transfers_same_cancel_and_capacity_before_say() {
    let (h, fixture, wake) = demand_harness();
    let (settle_native, hold_native) = oneshot::channel();
    *fixture.browser.hold_native.lock().unwrap() = Some(hold_native);
    let mut request = request(&h);
    request
        .attachments
        .push(PageAssistAttachV1::Page { tab: TAB.into() });
    let original = request.text.clone();
    let service = h.service.clone();
    let pending = tokio::spawn(async move { service.ask(request).await });
    until(|| fixture.holding.load(Ordering::Acquire)).await;
    assert!(h.service.state().running.is_empty());
    assert!(h
        .service
        .state()
        .live
        .values()
        .all(|live| live.conversation.messages().is_empty()));
    assert!(h.sink.0.lock().unwrap().is_empty());
    assert!(h.page.lock().reads == 0);
    let deadline = fixture.deadline.lock().unwrap().unwrap();
    assert!(deadline.duration_since(Instant::now()) <= Duration::from_secs(120));
    wake.send(()).unwrap();
    let (_, task) = pending.await.unwrap().unwrap();
    let native_cancel = fixture.cancel.lock().unwrap().clone().unwrap();
    {
        let state = h.service.state();
        assert!(state.preparing.is_empty());
        assert!(Arc::ptr_eq(
            &state.running.values().next().unwrap().cancel.native,
            &native_cancel
        ));
    }
    assert_eq!(fixture.browser.drafts.lock().unwrap()[0].text, original);
    settle_native.send(()).unwrap();
    task.await;
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
    assert!(fixture.browser.inputs.lock().unwrap()[0].contains("<<<PAGE a"));
    assert_eq!(fixture.prepares.load(Ordering::Acquire), 1);
    assert_eq!(fixture.releases.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn stop_during_wake_and_duplicate_ask_never_append_or_claim_not_sent() {
    let (h, fixture, _wake) = demand_harness();
    let request = request(&h);
    let service = h.service.clone();
    let copied = request.clone();
    let pending = tokio::spawn(async move { service.ask(copied).await });
    until(|| fixture.holding.load(Ordering::Acquire)).await;
    assert!(matches!(
        h.service.ask(request).await,
        Err(PageAssistFaultV1::Busy)
    ));
    assert!(h.service.stop(SPACE));
    assert!(matches!(
        pending.await.unwrap(),
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Cancelled
        })
    ));
    assert!(fixture
        .cancel
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .load(Ordering::Acquire));
    assert_eq!(fixture.prepares.load(Ordering::Acquire), 1);
    assert_eq!(fixture.releases.load(Ordering::Acquire), 1);
    assert_unsent(&h, &fixture);
}

#[tokio::test]
async fn drop_pending_ask_releases_only_its_preparing_owner() {
    let (h, fixture, _wake) = demand_harness();
    let request = request(&h);
    let service = h.service.clone();
    let pending = tokio::spawn(async move { service.ask(request).await });
    until(|| fixture.holding.load(Ordering::Acquire)).await;
    pending.abort();
    assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
    assert_eq!(fixture.releases.load(Ordering::Acquire), 1);
    assert_unsent(&h, &fixture);
}

#[tokio::test]
async fn close_new_conversation_end_all_and_scope_change_cancel_presay_owner() {
    for mode in 0..4 {
        let (h, fixture, _wake) = demand_harness();
        let request = request(&h);
        let service = h.service.clone();
        let pending = tokio::spawn(async move { service.ask(request).await });
        until(|| fixture.holding.load(Ordering::Acquire)).await;
        match mode {
            0 => h.service.close(SPACE),
            1 => {
                assert!(h.service.new_conversation(SPACE));
            }
            2 => h.service.end_all(),
            _ => {
                h.service.current(SPACE, ProfileId::from(9));
            }
        }
        assert!(matches!(
            pending.await.unwrap(),
            Err(PageAssistFaultV1::NativeConversation {
                fault: PageAssistNativeFaultV1::ScopeChanged
            })
        ));
        assert_eq!(fixture.releases.load(Ordering::Acquire), 1);
        assert!(!fixture.browser.committed.load(Ordering::Acquire));
        assert!(h.service.state().preparing.is_empty());
    }
}

#[tokio::test]
async fn source_change_during_wake_drops_demand_without_legacy_fallback() {
    let (h, fixture, wake) = demand_harness();
    let request = request(&h);
    let service = h.service.clone();
    let pending = tokio::spawn(async move { service.ask(request).await });
    until(|| fixture.holding.load(Ordering::Acquire)).await;
    fixture.switch_to_model.store(true, Ordering::Release);
    wake.send(()).unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::ScopeChanged
        })
    ));
    assert_eq!(fixture.releases.load(Ordering::Acquire), 1);
    assert_unsent(&h, &fixture);
}

#[tokio::test]
async fn cancellation_during_refinement_keeps_original_demand_and_no_visible_reply() {
    let (h, fixture, wake) = demand_harness();
    let (_release, wait) = oneshot::channel();
    *fixture.reserve_wait.lock().unwrap() = Some(wait);
    let request = request(&h);
    let service = h.service.clone();
    let pending = tokio::spawn(async move { service.ask(request).await });
    until(|| fixture.holding.load(Ordering::Acquire)).await;
    wake.send(()).unwrap();
    until(|| fixture.reserve_entered.load(Ordering::Acquire)).await;
    assert!(fixture.holding.load(Ordering::Acquire));
    assert!(h.service.stop(SPACE));
    assert!(matches!(
        pending.await.unwrap(),
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Cancelled
        })
    ));
    assert_eq!(fixture.releases.load(Ordering::Acquire), 1);
    assert_unsent(&h, &fixture);
}

#[tokio::test]
async fn preparation_error_preserves_draft_and_substantive_failure_without_retry() {
    let (h, fixture, wake) = demand_harness();
    *fixture.fail.lock().unwrap() = Some(PageAssistNativeFaultV1::Protocol);
    wake.send(()).unwrap();
    assert!(matches!(
        h.service.ask(request(&h)).await,
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Protocol
        })
    ));
    assert_eq!(fixture.prepares.load(Ordering::Acquire), 1);
    assert_unsent(&h, &fixture);
}

#[tokio::test]
async fn stale_preparation_guard_never_removes_successor_identity() {
    let h = harness(vec![]);
    let id = h.service.current(SPACE, profile());
    let old = super::super::super::preparation::Preparation::begin(&h.service, SPACE, &id).unwrap();
    h.service.state().preparing.remove(&id);
    let new = super::super::super::preparation::Preparation::begin(&h.service, SPACE, &id).unwrap();
    drop(old);
    assert!(h
        .service
        .state()
        .preparing
        .get(&id)
        .is_some_and(|p| Arc::ptr_eq(&p.cancel, &new.cancel)));
    assert!(!new.cancel.native.load(Ordering::Acquire));
    drop(new);
    assert!(h.service.state().preparing.is_empty());
}

#[tokio::test]
async fn original_preparation_deadline_cannot_be_extended_by_reservation_bound() {
    let h = harness(vec![]);
    let id = h.service.current(SPACE, profile());
    let mut preparing =
        super::super::super::preparation::Preparation::begin(&h.service, SPACE, &id).unwrap();
    preparing.native_demand();
    preparing.deadline = Instant::now() - Duration::from_millis(1);
    let polled = AtomicBool::new(false);
    let result = preparing
        .wait_until(Instant::now() + Duration::from_secs(8), async {
            polled.store(true, Ordering::Release);
        })
        .await;
    assert_eq!(
        result,
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Timeout
        })
    );
    assert!(!polled.load(Ordering::Acquire));
}

#[tokio::test]
async fn fresh_entry_await_rechecks_ai_revision_and_foreground_epoch() {
    for changed in 0..3 {
        let (h, fixture, wake) = demand_harness();
        let (release, wait) = oneshot::channel();
        *fixture.entry_wait.lock().unwrap() = Some(wait);
        let request = request(&h);
        let conversation = request.conversation.clone();
        let service = h.service.clone();
        let pending = tokio::spawn(async move { service.ask(request).await });
        until(|| fixture.holding.load(Ordering::Acquire)).await;
        wake.send(()).unwrap();
        until(|| fixture.entry_entered.load(Ordering::Acquire)).await;
        match changed {
            0 => h.ai.0.store(false, Ordering::Release),
            1 => {
                h.service
                    .state()
                    .live
                    .get_mut(&conversation)
                    .unwrap()
                    .revision = Arc::new(())
            }
            _ => {
                h.service
                    .state()
                    .current_epochs
                    .insert(SPACE.into(), Arc::new(()));
            }
        }
        release.send(()).unwrap();
        let result = pending.await.unwrap();
        if changed == 0 {
            assert!(matches!(result, Err(PageAssistFaultV1::AiOff)));
        } else {
            assert!(matches!(
                result,
                Err(PageAssistFaultV1::NativeConversation {
                    fault: PageAssistNativeFaultV1::ScopeChanged
                })
            ));
        }
        assert_eq!(fixture.releases.load(Ordering::Acquire), 1);
        assert_unsent(&h, &fixture);
    }
}

#[tokio::test]
async fn malformed_empty_draft_never_wakes_or_creates_preparing_history() {
    let (h, fixture, _wake) = demand_harness();
    let mut request = request(&h);
    request.text = " \r\n ".into();
    assert!(matches!(
        h.service.ask(request).await,
        Err(PageAssistFaultV1::Invalid)
    ));
    assert_eq!(fixture.prepares.load(Ordering::Acquire), 0);
    assert_unsent(&h, &fixture);
}

struct HeldLegacy {
    base: Arc<dyn ModelSource>,
    entered: Arc<AtomicBool>,
    wait: Mutex<Option<oneshot::Receiver<()>>>,
}
impl ModelSource for HeldLegacy {
    fn entry<'a>(&'a self, profile: &'a ProfileId) -> BoxFuture<'a, Option<PageBackendChoice>> {
        Box::pin(async move {
            let wait = self.wait.lock().unwrap().take();
            if let Some(wait) = wait {
                self.entered.store(true, Ordering::Release);
                let _ = wait.await;
            }
            self.base.entry(profile).await
        })
    }
    fn resolve<'a>(
        &'a self,
        profile: &'a ProfileId,
        source: WorkCallSource,
    ) -> BoxFuture<'a, Result<ResolvedPageBackend, PageBackendError>> {
        self.base.resolve(profile, source)
    }
}
#[tokio::test(start_paused = true)]
async fn legacy_model_held_entry_has_no_native_prepare_deadline_or_wake() {
    let mut h = harness(vec![say("legacy")]);
    let (release, wait) = oneshot::channel();
    let entered = Arc::new(AtomicBool::new(false));
    let mut ports = h.service.ports().clone();
    ports.models = Arc::new(HeldLegacy {
        base: ports.models.clone(),
        entered: entered.clone(),
        wait: Mutex::new(Some(wait)),
    });
    h.service = PageAssist::new(ports);
    let request = request(&h);
    let service = h.service.clone();
    let pending = tokio::spawn(async move { service.ask(request).await });
    until(|| entered.load(Ordering::Acquire)).await;
    tokio::time::advance(Duration::from_secs(121)).await;
    tokio::task::yield_now().await;
    assert!(
        !pending.is_finished(),
        "native bootstrap timeout must not change legacy Model timing"
    );
    assert!(h.service.state().running.is_empty());
    assert!(h.client.requests.lock().unwrap().is_empty());
    release.send(()).unwrap();
    let (_, task) = pending.await.unwrap().unwrap();
    task.await;
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
    assert_eq!(h.client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stop_at_preparing_to_running_boundary_uses_same_writer_cancel() {
    let (h, fixture, wake) = demand_harness();
    wake.send(()).unwrap();
    // Every preparation await is ready in this poll. No spawned Browser owner
    // has run yet when ask returns its synchronously committed handoff.
    let (_, task) = h.service.ask(request(&h)).await.unwrap();
    assert!(h.service.state().preparing.is_empty());
    assert_eq!(h.service.state().running.len(), 1);
    let same = fixture.cancel.lock().unwrap().clone().unwrap();
    assert!(h.service.stop(SPACE));
    assert!(same.load(Ordering::Acquire));
    task.await;
    assert!(!fixture.browser.provider_started.load(Ordering::Acquire));
    assert!(fixture.browser.local_settled.load(Ordering::Acquire));
    assert!(fixture.browser.native_settled.load(Ordering::Acquire));
    assert!(h.page.lock().acted.is_empty());
}

#[tokio::test]
async fn expired_original_prepare_deadline_never_polls_native_wake_or_reserve() {
    let (h, fixture, _wake) = demand_harness();
    let id = h.service.current(SPACE, profile());
    let mut preparing =
        super::super::super::preparation::Preparation::begin(&h.service, SPACE, &id).unwrap();
    preparing.deadline = Instant::now() - Duration::from_millis(1);
    let profile = profile();
    let result = preparing
        .wait_until(
            preparing.deadline,
            h.service.ports().models.prepare_reply(
                &profile,
                preparing.cancel.native.clone(),
                preparing.deadline,
            ),
        )
        .await;
    assert!(matches!(
        result,
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Timeout
        })
    ));
    assert_eq!(fixture.prepares.load(Ordering::Acquire), 0);
    drop(preparing);
    assert_unsent(&h, &fixture);
}
