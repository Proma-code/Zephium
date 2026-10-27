//! Full Page composition fixtures; no original runtime, account, or network.
use super::*;

struct BrowserFixture {
    reserved: AtomicBool,
    policy_mismatch: AtomicBool,
    validate_failure: Mutex<Option<PageAssistNativeFaultV1>>,
    overflow_callback: AtomicBool,
    committed: AtomicBool,
    provider_started: AtomicBool,
    local_settled: AtomicBool,
    native_settled: AtomicBool,
    completion_settlement: Mutex<NativeBrowserSettlement>,
    provider_error: Mutex<Option<PageAssistNativeFaultV1>>,
    availability_wait: Mutex<Option<oneshot::Receiver<()>>>,
    availability_entered: AtomicBool,
    seal_queued_request: AtomicBool,
    drafts: Mutex<Vec<NativeConversationDraft>>,
    inputs: Mutex<Vec<String>>,
    outputs: Mutex<Vec<(bool, String)>>,
    calls: Vec<(String, Value)>,
    final_text: String,
    timing: Mutex<Option<NativeBudgetView>>,
    revoke: Mutex<Option<NativeBrowserRevocation>>,
    hold_native: Mutex<Option<oneshot::Receiver<()>>>,
    sealed_outcome: Mutex<Option<NativeConversationOutcome>>,
    early_result: Mutex<
        Option<oneshot::Receiver<Result<NativeConversationOutcome, PageAssistNativeFaultV1>>>,
    >,
}
fn browser_choice() -> NativeConversationChoice {
    NativeConversationChoice {
        capabilities: PageAssistCapabilitiesV1::Page,
        ..native_choice()
    }
}
struct BrowserModels(Arc<BrowserFixture>);
struct BrowserReservation {
    fixture: Arc<BrowserFixture>,
    handed: bool,
}
impl Drop for BrowserReservation {
    fn drop(&mut self) {
        if !self.handed {
            self.fixture.reserved.store(false, Ordering::Release);
        }
    }
}
impl NativeConversationReservation for BrowserReservation {
    fn validate(&self) -> Result<(), PageAssistNativeFaultV1> {
        self.fixture
            .validate_failure
            .lock()
            .unwrap()
            .map_or(Ok(()), Err)
    }
    fn commit(
        self: Box<Self>,
        _: u32,
        _: Arc<AtomicBool>,
        _: NativeConversationEvents,
    ) -> BoxFuture<'static, Result<NativeConversationOutcome, PageAssistNativeFaultV1>> {
        panic!("BrowserV1 cannot fall back to text")
    }
    fn commit_browser(
        mut self: Box<Self>,
        _: u32,
        cancel: Arc<AtomicBool>,
        events: NativeConversationEvents,
        browser: NativeBrowserBridge,
    ) -> BoxFuture<'static, Result<NativeBrowserCompletion, PageAssistNativeFaultV1>> {
        self.handed = true;
        let fixture = self.fixture.clone();
        fixture.committed.store(true, Ordering::Release);
        let (prepared, requests, timing, local, revoke) = browser.into_parts();
        *fixture.revoke.lock().unwrap() = Some(revoke);
        *fixture.timing.lock().unwrap() = Some(timing.clone());
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let mut early_result = None;
            let early = fixture.early_result.lock().unwrap().take();
            let input = if let Some(early) = early {
                let mut prepared = prepared;
                tokio::select! {
                    input = &mut prepared => input,
                    result = early => {
                        early_result = Some(result.unwrap());
                        fixture.revoke.lock().unwrap().as_ref().unwrap().close_admission();
                        prepared.await
                    }
                }
            } else {
                prepared.await
            };
            let started = match input {
                Ok(Ok(input)) if !cancel.load(Ordering::Acquire) && timing.deadline().is_ok() => {
                    fixture.inputs.lock().unwrap().push(input.into_text());
                    fixture.provider_started.store(true, Ordering::Release);
                    true
                }
                _ => false,
            };
            if started {
                if fixture.overflow_callback.load(Ordering::Acquire) {
                    assert_eq!(
                        events(NativeConversationEvent::TextSnapshot {
                            text: "x".repeat(zephium_page_assist::conversation::MAX_TEXT_BYTES + 1),
                        }),
                        Err(PageAssistNativeFaultV1::Limit)
                    );
                }
                for (index, (name, arguments)) in fixture.calls.iter().enumerate() {
                    if cancel.load(Ordering::Acquire) {
                        break;
                    }
                    let (request, response) = NativeBrowserToolRequest::channel(
                        index as u32 + 1,
                        name.clone(),
                        arguments.clone(),
                    )
                    .unwrap();
                    if requests.send(request).await.is_err() {
                        break;
                    }
                    if fixture.seal_queued_request.load(Ordering::Acquire) {
                        fixture
                            .revoke
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .close_admission();
                    }
                    match response.await {
                        Ok(Ok(output)) => fixture
                            .outputs
                            .lock()
                            .unwrap()
                            .push((output.success(), output.text().to_owned())),
                        _ => break,
                    }
                }
                if !cancel.load(Ordering::Acquire) {
                    let _ = events(NativeConversationEvent::TextSnapshot {
                        text: fixture.final_text.clone(),
                    });
                }
            }
            // Original normal terminal seals Page admission, without setting
            // the independent Stop bit or a substantive revocation fault.
            fixture
                .revoke
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .close_admission();
            drop(requests);
            local
                .await
                .expect("a dropped channel is not local settlement");
            fixture.local_settled.store(true, Ordering::Release);
            let hold = fixture.hold_native.lock().unwrap().take();
            if let Some(hold) = hold {
                let _ = hold.await;
            }
            let settlement = *fixture.completion_settlement.lock().unwrap();
            if matches!(settlement, NativeBrowserSettlement::Joined { .. }) {
                fixture.native_settled.store(true, Ordering::Release);
                fixture.reserved.store(false, Ordering::Release);
            }
            let status = if !started {
                NativeConversationStatus::NeverStarted
            } else if cancel.load(Ordering::Acquire) {
                NativeConversationStatus::Interrupted
            } else {
                NativeConversationStatus::Completed
            };
            let outcome = fixture
                .sealed_outcome
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| NativeConversationOutcome {
                    status,
                    final_response: (status == NativeConversationStatus::Completed)
                        .then(|| fixture.final_text.clone()),
                });
            let result = early_result
                .unwrap_or_else(|| match fixture.provider_error.lock().unwrap().take() {
                    Some(error) => Err(error),
                    None => Ok(outcome),
                })
                .map(|outcome| NativeBrowserCompletion {
                    outcome,
                    settlement,
                });
            let _ = send.send(result);
        });
        Box::pin(async move {
            receive
                .await
                .map_err(|_| PageAssistNativeFaultV1::OutcomeUnknown)?
        })
    }
}
struct BrowserDemand(Arc<BrowserFixture>);
impl NativeConversationDemand for BrowserDemand {
    fn reserve_browser(
        self: Box<Self>,
        expected: NativeConversationChoice,
        draft: NativeConversationDraft,
        manifest: NativeBrowserManifest,
    ) -> BoxFuture<'static, Result<Box<dyn NativeConversationReservation>, PageBackendError>> {
        Box::pin(async move {
            BrowserModels(self.0)
                .reserve_native_browser(&profile(), expected, draft, manifest)
                .await
        })
    }
}
impl ModelSource for BrowserModels {
    fn prepare_reply<'a>(
        &'a self,
        _: &'a ProfileId,
        _: Arc<AtomicBool>,
        _: std::time::Instant,
    ) -> BoxFuture<'a, Result<Option<Box<dyn NativeConversationDemand>>, PageBackendError>> {
        Box::pin(async {
            Ok(Some(
                Box::new(BrowserDemand(self.0.clone())) as Box<dyn NativeConversationDemand>
            ))
        })
    }

    fn entry<'a>(&'a self, _: &'a ProfileId) -> BoxFuture<'a, Option<PageBackendChoice>> {
        Box::pin(async move {
            // Only post-commit availability is held; pre-say admission remains
            // unchanged and must not itself manufacture a pending Page tool.
            let wait = if self.0.committed.load(Ordering::Acquire) {
                self.0.availability_wait.lock().unwrap().take()
            } else {
                None
            };
            if let Some(wait) = wait {
                self.0.availability_entered.store(true, Ordering::Release);
                let _ = wait.await;
            }
            Some(PageBackendChoice::NativeConversation(browser_choice()))
        })
    }
    fn resolve<'a>(
        &'a self,
        _: &'a ProfileId,
        _: WorkCallSource,
    ) -> BoxFuture<'a, Result<ResolvedPageBackend, PageBackendError>> {
        panic!("BrowserV1 does not emulate WorkModelClient")
    }
    fn reserve_native_browser<'a>(
        &'a self,
        _: &'a ProfileId,
        expected: NativeConversationChoice,
        draft: NativeConversationDraft,
        manifest: NativeBrowserManifest,
    ) -> BoxFuture<'a, Result<Box<dyn NativeConversationReservation>, PageBackendError>> {
        Box::pin(async move {
            assert_eq!(expected, browser_choice());
            assert_eq!(manifest.version(), "zephium-page-browser-v1");
            assert_eq!(manifest.instructions(), zephium_page_assist::prompt::SYSTEM);
            assert_eq!(manifest.tools().len(), 9);
            if self.0.policy_mismatch.load(Ordering::Acquire) {
                return Err(PageBackendError::NativeConversation(
                    PageAssistNativeFaultV1::HistoryDiverged,
                ));
            }
            if self.0.reserved.swap(true, Ordering::AcqRel) {
                return Err(PageBackendError::Busy);
            }
            self.0.drafts.lock().unwrap().push(draft);
            Ok(Box::new(BrowserReservation {
                fixture: self.0.clone(),
                handed: false,
            }) as Box<dyn NativeConversationReservation>)
        })
    }
}
fn browser_harness(calls: Vec<(&str, Value)>, final_text: &str) -> (Harness, Arc<BrowserFixture>) {
    let mut h = harness(vec![]);
    let fixture = Arc::new(BrowserFixture {
        reserved: AtomicBool::new(false),
        policy_mismatch: AtomicBool::new(false),
        validate_failure: Mutex::new(None),
        overflow_callback: AtomicBool::new(false),
        committed: AtomicBool::new(false),
        provider_started: AtomicBool::new(false),
        local_settled: AtomicBool::new(false),
        native_settled: AtomicBool::new(false),
        completion_settlement: Mutex::new(NativeBrowserSettlement::Joined {
            local: NativeBrowserLocalOutcome::Observed,
        }),
        provider_error: Mutex::new(None),
        availability_wait: Mutex::new(None),
        availability_entered: AtomicBool::new(false),
        seal_queued_request: AtomicBool::new(false),
        drafts: Mutex::new(Vec::new()),
        inputs: Mutex::new(Vec::new()),
        outputs: Mutex::new(Vec::new()),
        calls: calls
            .into_iter()
            .map(|(name, args)| (name.into(), args))
            .collect(),
        final_text: final_text.into(),
        timing: Mutex::new(None),
        revoke: Mutex::new(None),
        hold_native: Mutex::new(None),
        sealed_outcome: Mutex::new(None),
        early_result: Mutex::new(None),
    });
    let mut ports = h.service.ports().clone();
    ports.models = Arc::new(BrowserModels(fixture.clone()));
    h.service = PageAssist::new(ports);
    (h, fixture)
}
async fn ask_browser(h: &Harness, words: &str, attach: bool) -> (u32, TurnTask) {
    let mut request = native_ask(h.service.current(SPACE, profile()), words);
    if attach {
        request
            .attachments
            .push(PageAssistAttachV1::Page { tab: TAB.into() });
    }
    h.service.ask_native_mode(request, true).await.unwrap()
}
async fn until(mut condition: impl FnMut() -> bool) {
    for _ in 0..300 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("fixture condition did not settle");
}

#[tokio::test]
async fn browser_context_and_original_read_tool_produce_cited_final_answer() {
    let (h, fixture) = browser_harness(
        vec![("list_tabs", json!({})), ("read_page", json!({"tab":"t1"}))],
        "The certificate story [a1].",
    );
    let (_, task) = ask_browser(&h, "  Read this page\r\nexact words  ", true).await;
    assert!(
        fixture.committed.load(Ordering::Acquire),
        "handoff is synchronous before task poll"
    );
    task.await;
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
    let input = fixture.inputs.lock().unwrap()[0].clone();
    assert!(input.contains("<<<PAGE a\nhost: news.ycombinator.com"));
    assert!(input.ends_with("  Read this page\r\nexact words  "));
    assert!(!input.contains(PASSWORD));
    assert_eq!(input.matches("PAGE>>>").count(), 1);
    assert_eq!(fixture.outputs.lock().unwrap().len(), 2);
    let blocks = h.blocks().await;
    let PageAssistBlockV1::Text { text, cites } = &blocks[0] else {
        panic!()
    };
    assert!(text.contains('⟦'));
    assert_eq!(cites.len(), 1);
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.native_settled.load(Ordering::Acquire));
    assert!(h.service.state().native_owners.is_empty());
    assert!(h.requests().is_empty());
}

#[tokio::test]
async fn browser_history_policy_mismatch_refuses_before_visible_say() {
    let (h, fixture) = browser_harness(vec![], "");
    let conversation = h.conversation().await;
    let before = h.service.status(SPACE, Some(TAB)).await.conversation;
    fixture.policy_mismatch.store(true, Ordering::Release);
    assert!(matches!(
        h.service
            .ask_native_mode(native_ask(conversation, "do not append"), true)
            .await,
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::HistoryDiverged
        })
    ));
    assert_eq!(
        h.service.status(SPACE, Some(TAB)).await.conversation,
        before
    );
    assert!(!fixture.committed.load(Ordering::Acquire));
    assert!(!fixture.provider_started.load(Ordering::Acquire));
}

#[tokio::test]
async fn browser_unpolled_waiter_drop_retains_owner_until_no_send_and_both_returns() {
    let (h, fixture) = browser_harness(vec![], "unused");
    let (release, hold) = oneshot::channel();
    *fixture.hold_native.lock().unwrap() = Some(hold);
    let (_, task) = ask_browser(&h, "cancel before preparation", false).await;
    drop(task);
    until(|| fixture.local_settled.load(Ordering::Acquire)).await;
    assert!(!fixture.provider_started.load(Ordering::Acquire));
    assert!(fixture.reserved.load(Ordering::Acquire));
    assert_eq!(h.service.state().native_owners.len(), 1);
    release.send(()).unwrap();
    until(|| h.service.state().native_owners.is_empty()).await;
    assert!(fixture.native_settled.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn browser_actual_confirmation_pauses_active_budget_but_never_approves_action() {
    let (h, fixture) = browser_harness(
        vec![("act", json!({"steps":[{"verb":"click","ref":"a6"}]}))],
        "Done",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Place order".into(),
            effect: StepEffect::Commit(WorkConfirmCategoryV1::Purchase),
        },
    );
    let (reply, task) = ask_browser(&h, "Order", true).await;
    until(|| {
        h.service
            .state()
            .running
            .values()
            .any(|r| r.decision.is_some())
    })
    .await;
    tokio::time::advance(Duration::from_secs(120)).await;
    tokio::task::yield_now().await;
    assert!(h.page.lock().acted.is_empty());
    assert!(fixture
        .timing
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .deadline()
        .is_ok());
    let conversation = h.conversation().await;
    assert!(h
        .service
        .decide(&conversation, reply, 1, PageAssistDecisionV1::Approve));
    task.await;
    assert_eq!(h.page.lock().acted.len(), 1);
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
}

#[tokio::test]
async fn browser_stop_during_entered_effect_retains_both_owners_until_actual_return() {
    let (h, fixture) = browser_harness(
        vec![(
            "act",
            json!({"steps":[{"verb":"click","ref":"a6"},{"verb":"click","ref":"a6"}]}),
        )],
        "unused",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Expand".into(),
            effect: StepEffect::Read,
        },
    );
    let (release, hold) = oneshot::channel();
    h.page.lock().act_wait = Some(hold);
    let (_, task) = ask_browser(&h, "Expand twice", true).await;
    until(|| h.page.lock().act_entered).await;
    assert!(h.service.stop(SPACE));
    drop(task);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!h.page.lock().act_returned);
    assert!(!h.page.lock().act_dropped.load(Ordering::Acquire));
    assert!(!fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.reserved.load(Ordering::Acquire));
    assert_eq!(h.service.state().native_owners.len(), 1);
    release.send(()).unwrap();
    until(|| h.service.state().native_owners.is_empty()).await;
    assert!(h.page.lock().act_returned);
    assert_eq!(
        h.page.lock().acted.len(),
        1,
        "cancelled reply must not start the second step"
    );
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.native_settled.load(Ordering::Acquire));
}

#[tokio::test]
async fn browser_invalid_arguments_and_secret_target_never_bypass_original_authority() {
    let (h, fixture) = browser_harness(
        vec![
            (
                "act",
                json!({"steps":[{"verb":"fill","ref":"a5","value":"never"}]}),
            ),
            ("read_page", json!({"wrong":"shape"})),
        ],
        "Refused",
    );
    let (_, task) = ask_browser(&h, "Read safely", true).await;
    task.await;
    assert!(h.page.lock().acted.is_empty());
    assert_eq!(fixture.outputs.lock().unwrap().len(), 2);
    assert!(fixture
        .outputs
        .lock()
        .unwrap()
        .iter()
        .all(|(success, _)| !success));
    assert!(!fixture.inputs.lock().unwrap()[0].contains(PASSWORD));
}

#[tokio::test]
async fn browser_callback_fault_is_not_masked_by_actor_cancellation() {
    let (h, fixture) = browser_harness(vec![], "unused");
    fixture.overflow_callback.store(true, Ordering::Release);
    let (_, task) = ask_browser(&h, "overflow fixture", false).await;
    task.await;
    let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Limit
        })
    );
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.native_settled.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn browser_confirmation_exhaustion_cannot_approve_or_reset_human_budget() {
    let (h, fixture) = browser_harness(
        vec![("act", json!({"steps":[{"verb":"click","ref":"a6"}]}))],
        "unused",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Place order".into(),
            effect: StepEffect::Commit(WorkConfirmCategoryV1::Purchase),
        },
    );
    let (_, task) = ask_browser(&h, "Order", true).await;
    until(|| {
        h.service
            .state()
            .running
            .values()
            .any(|r| r.decision.is_some())
    })
    .await;
    tokio::time::advance(Duration::from_secs(300)).await;
    task.await;
    assert!(h.page.lock().acted.is_empty());
    let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Timeout
        })
    );
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.native_settled.load(Ordering::Acquire));
}

#[tokio::test]
async fn browser_declined_confirmation_returns_original_failure_without_effect() {
    let (h, fixture) = browser_harness(
        vec![("act", json!({"steps":[{"verb":"click","ref":"a6"}]}))],
        "No changes",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Place order".into(),
            effect: StepEffect::Commit(WorkConfirmCategoryV1::Purchase),
        },
    );
    let (reply, task) = ask_browser(&h, "Order", true).await;
    until(|| {
        h.service
            .state()
            .running
            .values()
            .any(|r| r.decision.is_some())
    })
    .await;
    assert!(h.service.decide(
        &h.conversation().await,
        reply,
        1,
        PageAssistDecisionV1::Decline
    ));
    task.await;
    assert!(h.page.lock().acted.is_empty());
    assert!(!fixture.outputs.lock().unwrap()[0].0);
    assert!(fixture.outputs.lock().unwrap()[0].1.contains("declined"));
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
}

#[tokio::test]
async fn browser_style_remains_a_proposal_until_original_apply_action() {
    let (h, fixture) = browser_harness(
        vec![(
            "restyle",
            json!({"page":"a","summary":"Readable","css":"body{background-color:#fafafa!important;color:#111111!important}"}),
        )],
        "A proposed style",
    );
    let (reply, task) = ask_browser(&h, "Restyle", true).await;
    task.await;
    assert!(h.page.lock().applied.is_empty());
    let PageAssistBlockV1::Style { style } = &h.blocks().await[1] else {
        panic!()
    };
    assert_eq!(style.state, PageStyleStateV1::Proposed);
    assert!(h.service.decide(
        &h.conversation().await,
        reply,
        1,
        PageAssistDecisionV1::Approve
    ));
    assert_eq!(h.page.lock().applied.len(), 1);
    assert!(fixture.outputs.lock().unwrap()[0].0);
}

#[tokio::test]
async fn browser_cards_and_open_tabs_keep_observed_reference_authority() {
    let (h, fixture) = browser_harness(
        vec![
            (
                "show_items",
                json!({"items":[{"ref":"a1","title":"Certificate story"}]}),
            ),
            (
                "open_tabs",
                json!({"targets":["a1","https://not-observed.invalid/"]}),
            ),
        ],
        "Opened the observed story",
    );
    let (_, task) = ask_browser(&h, "Show and open the first story", true).await;
    task.await;
    assert_eq!(*h.tabs.0.lock().unwrap(), vec!["https://emudhra.com/exit"]);
    assert!(h
        .blocks()
        .await
        .iter()
        .any(|block| matches!(block, PageAssistBlockV1::Items { .. })));
    assert!(h
        .blocks()
        .await
        .iter()
        .any(|block| matches!(block, PageAssistBlockV1::Opened { .. })));
    assert_eq!(fixture.outputs.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn browser_no_tool_after_handoff_and_no_implicit_work_start() {
    let (h, fixture) = browser_harness(
        vec![
            (
                "hand_off",
                json!({"title":"Research","summary":"A longer comparison","tabs":["t1"]}),
            ),
            ("open_tabs", json!({"targets":["a1"]})),
        ],
        "Offered to you",
    );
    let (_, task) = ask_browser(&h, "Prepare a larger job", true).await;
    task.await;
    assert!(h.work.0.lock().unwrap().is_empty());
    assert!(h.tabs.0.lock().unwrap().is_empty());
    assert!(h
        .blocks()
        .await
        .iter()
        .any(|block| matches!(block, PageAssistBlockV1::Handoff { .. })));
    assert_eq!(fixture.outputs.lock().unwrap().len(), 2);
    assert!(!fixture.outputs.lock().unwrap()[1].0);
}

#[tokio::test]
async fn browser_native_account_revocation_wakes_confirmation_and_cannot_approve() {
    let (h, fixture) = browser_harness(
        vec![("act", json!({"steps":[{"verb":"click","ref":"a6"}]}))],
        "unused",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Place order".into(),
            effect: StepEffect::Commit(WorkConfirmCategoryV1::Purchase),
        },
    );
    let (reply, task) = ask_browser(&h, "Order", true).await;
    until(|| {
        h.service
            .state()
            .running
            .values()
            .any(|r| r.decision.is_some())
    })
    .await;
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .revoke(NativeBrowserRevokeReason::ScopeChanged);
    // Even an already queued person click cannot restore the revoked scope.
    let _ = h.service.decide(
        &h.conversation().await,
        reply,
        1,
        PageAssistDecisionV1::Approve,
    );
    task.await;
    assert!(h.page.lock().acted.is_empty());
    let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::ScopeChanged
        })
    );
    assert!(fixture.local_settled.load(Ordering::Acquire));
}

#[tokio::test]
async fn browser_native_revocation_retains_entered_effect_and_blocks_next_step() {
    let (h, fixture) = browser_harness(
        vec![(
            "act",
            json!({"steps":[{"verb":"click","ref":"a6"},{"verb":"click","ref":"a6"}]}),
        )],
        "unused",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Expand".into(),
            effect: StepEffect::Read,
        },
    );
    let (release, hold) = oneshot::channel();
    h.page.lock().act_wait = Some(hold);
    let (_, task) = ask_browser(&h, "Expand twice", true).await;
    until(|| h.page.lock().act_entered).await;
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .revoke(NativeBrowserRevokeReason::ScopeChanged);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!h.page.lock().act_dropped.load(Ordering::Acquire));
    assert!(!fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.reserved.load(Ordering::Acquire));
    release.send(()).unwrap();
    task.await;
    assert_eq!(h.page.lock().acted.len(), 1);
    assert!(h.page.lock().act_returned);
    let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::ScopeChanged
        })
    );
    assert!(fixture.local_settled.load(Ordering::Acquire));
}

async fn held_browser_effect() -> (Harness, Arc<BrowserFixture>, oneshot::Sender<()>, TurnTask) {
    let (h, fixture) = browser_harness(
        vec![(
            "act",
            json!({"steps":[
                {"verb":"click","ref":"a6"},{"verb":"click","ref":"a6"}
            ]}),
        )],
        "provisional",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Expand".into(),
            effect: StepEffect::Read,
        },
    );
    let (release, hold) = oneshot::channel();
    h.page.lock().act_wait = Some(hold);
    let (_, task) = ask_browser(&h, "Expand twice", true).await;
    until(|| h.page.lock().act_entered).await;
    (h, fixture, release, task)
}

#[tokio::test]
async fn browser_completed_then_revoked_keeps_final_text_after_actual_local_return() {
    let (h, fixture, release, task) = held_browser_effect().await;
    // The original terminal is known first. Same-pump account invalidation then
    // revokes Page admission while the already-entered browser future is owned.
    *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
        status: NativeConversationStatus::Completed,
        final_response: Some("Authentic final [a1].".into()),
    });
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .revoke(NativeBrowserRevokeReason::ScopeChanged);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!fixture.local_settled.load(Ordering::Acquire));
    assert!(!h.page.lock().act_dropped.load(Ordering::Acquire));
    assert!(fixture.reserved.load(Ordering::Acquire));
    release.send(()).unwrap();
    task.await;
    assert!(h.page.lock().act_returned);
    assert_eq!(h.page.lock().acted.len(), 1);
    let message = h.reply().await;
    let PageAssistMessageV1::Reply {
        state,
        fault,
        blocks,
        ..
    } = &message
    else {
        panic!()
    };
    assert_eq!(*state, PageAssistReplyStateV1::Failed);
    assert_eq!(
        *fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::LocalInterruption
        })
    );
    let PageAssistBlockV1::Text { text, cites } = &blocks[0] else {
        panic!()
    };
    assert!(text.starts_with("Authentic final "));
    assert_eq!(cites.len(), 1);
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.native_settled.load(Ordering::Acquire));
    let conversation = h.conversation().await;
    let snapshot = h.service.state().live[&conversation]
        .conversation
        .view()
        .clone();
    let stored: PageAssistConversationV1 =
        serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
    let restored = Conversation::restore(stored, SPACE.into());
    let PageAssistMessageV1::Reply {
        state,
        fault,
        blocks: restored_blocks,
        ..
    } = restored.messages().last().unwrap()
    else {
        panic!()
    };
    assert_eq!(*state, PageAssistReplyStateV1::Failed);
    assert_eq!(
        *fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::LocalInterruption
        })
    );
    assert_eq!(restored_blocks, blocks);
}

#[tokio::test]
async fn browser_sealed_unknown_and_history_uncertain_are_never_promoted_by_local_revocation() {
    for (status, expected) in [
        (
            NativeConversationStatus::Unknown,
            PageAssistNativeFaultV1::OutcomeUnknown,
        ),
        (
            NativeConversationStatus::HistoryUncertain,
            PageAssistNativeFaultV1::HistoryUncertain,
        ),
    ] {
        let (h, fixture, release, task) = held_browser_effect().await;
        *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
            status,
            final_response: Some("Known completed item, distinct terminal".into()),
        });
        fixture
            .revoke
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .revoke(NativeBrowserRevokeReason::ScopeChanged);
        release.send(()).unwrap();
        task.await;
        let PageAssistMessageV1::Reply {
            state,
            fault,
            blocks,
            ..
        } = h.reply().await
        else {
            panic!()
        };
        assert_eq!(state, PageAssistReplyStateV1::Failed);
        assert_eq!(
            fault,
            Some(PageAssistFaultV1::NativeConversation { fault: expected })
        );
        assert!(
            matches!(&blocks[0], PageAssistBlockV1::Text { text, .. } if text == "Known completed item, distinct terminal")
        );
        let conversation = h.conversation().await;
        let state = h.service.state();
        assert_eq!(
            state.live[&conversation]
                .replies
                .values()
                .next()
                .unwrap()
                .native_outcome
                .as_ref()
                .unwrap()
                .status,
            status
        );
    }
}

#[tokio::test]
async fn browser_sealed_terminal_refuses_newer_foreground_lifetime_profile_or_reply_owner() {
    for mismatch in 0..4 {
        let (h, fixture, release, task) = held_browser_effect().await;
        *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
            status: NativeConversationStatus::Completed,
            final_response: Some("MUST NOT ENTER NEW LIFETIME".into()),
        });
        let conversation = h.conversation().await;
        fixture
            .revoke
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .revoke(NativeBrowserRevokeReason::ScopeChanged);
        {
            let mut state = h.service.state();
            match mismatch {
                0 => {
                    state.current_epochs.insert(SPACE.into(), Arc::new(()));
                }
                1 => {
                    state.live.get_mut(&conversation).unwrap().profile = ProfileId::from(99);
                }
                2 => {
                    state.running.get_mut(&conversation).unwrap().cancel =
                        Arc::new(Cancel::default());
                }
                _ => {
                    state
                        .current
                        .insert(SPACE.into(), "another-conversation".into());
                }
            }
        }
        release.send(()).unwrap();
        task.await;
        let state = h.service.state();
        let live = &state.live[&conversation];
        assert!(live
            .replies
            .values()
            .all(|data| data.native_outcome.is_none()));
        assert!(!serde_json::to_string(live.conversation.view())
            .unwrap()
            .contains("MUST NOT ENTER"));
    }
}

#[tokio::test]
async fn browser_exact_early_never_started_retains_entered_context_preparation_until_return() {
    let (h, fixture) = browser_harness(vec![], "unused");
    let (release_read, read_wait) = oneshot::channel();
    h.page.lock().read_wait = Some(read_wait);
    let (finish_native, early) = oneshot::channel();
    *fixture.early_result.lock().unwrap() = Some(early);
    let (_, task) = ask_browser(&h, "No send", true).await;
    until(|| h.page.lock().read_entered).await;
    finish_native
        .send(Ok(NativeConversationOutcome {
            status: NativeConversationStatus::NeverStarted,
            final_response: None,
        }))
        .unwrap();
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!h.page.lock().read_returned);
    assert!(!h.page.lock().read_dropped.load(Ordering::Acquire));
    assert!(!fixture.local_settled.load(Ordering::Acquire));
    assert!(!h.service.state().native_owners.is_empty());
    release_read.send(()).unwrap();
    task.await;
    assert!(h.page.lock().read_returned);
    assert!(!fixture.provider_started.load(Ordering::Acquire));
    let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::NotSent
        })
    );
}

#[tokio::test]
async fn browser_early_error_or_cancel_never_fabricates_no_send_proof() {
    for error in [
        PageAssistNativeFaultV1::Cancelled,
        PageAssistNativeFaultV1::OutcomeUnknown,
    ] {
        let (h, fixture) = browser_harness(vec![], "unused");
        let (release_read, read_wait) = oneshot::channel();
        h.page.lock().read_wait = Some(read_wait);
        let (finish_native, early) = oneshot::channel();
        *fixture.early_result.lock().unwrap() = Some(early);
        let (_, task) = ask_browser(&h, "Unknown admission", true).await;
        until(|| h.page.lock().read_entered).await;
        finish_native.send(Err(error)).unwrap();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        release_read.send(()).unwrap();
        task.await;
        let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
            panic!()
        };
        assert_ne!(
            fault,
            Some(PageAssistFaultV1::NativeConversation {
                fault: PageAssistNativeFaultV1::NotSent
            })
        );
    }
}

#[tokio::test]
async fn browser_completed_empty_selection_clears_provisional_snapshot() {
    let (h, fixture) = browser_harness(vec![], "provisional only");
    *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
        status: NativeConversationStatus::Completed,
        final_response: None,
    });
    let (_, task) = ask_browser(&h, "Empty completion", false).await;
    task.await;
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
    assert!(
        matches!(&h.blocks().await[0], PageAssistBlockV1::Text { text, cites } if text.is_empty() && cites.is_empty())
    );
}

#[tokio::test]
async fn browser_sealed_oversized_final_stays_limit_after_revocation() {
    let (h, fixture, release, task) = held_browser_effect().await;
    *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
        status: NativeConversationStatus::Completed,
        final_response: Some("x".repeat(zephium_page_assist::conversation::MAX_TEXT_BYTES + 1)),
    });
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .revoke(NativeBrowserRevokeReason::ScopeChanged);
    release.send(()).unwrap();
    task.await;
    let PageAssistMessageV1::Reply { state, fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(state, PageAssistReplyStateV1::Failed);
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Limit
        })
    );
}

#[tokio::test]
async fn browser_claimed_no_send_with_final_text_is_protocol_not_not_sent() {
    let (h, fixture) = browser_harness(vec![], "");
    *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
        status: NativeConversationStatus::NeverStarted,
        final_response: Some("contradictory completed item".into()),
    });
    let (_, task) = ask_browser(&h, "Contradictory receipt", false).await;
    task.await;
    let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::Protocol
        })
    );
}

#[tokio::test]
async fn browser_choice_capability_is_preserved_without_text_fallback() {
    let (h, fixture) = browser_harness(vec![], "answer");
    let status = h.service.status(SPACE, Some(TAB)).await;
    assert_eq!(
        status.model.unwrap().capabilities,
        Some(PageAssistCapabilitiesV1::Page)
    );
    let before = status.conversation.clone();
    let refusal = h
        .service
        .ask_text_fixture(native_ask(before.id.clone(), "No silent text fallback"))
        .await;
    assert!(matches!(
        refusal,
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::UnsupportedInput
        })
    ));
    assert!(!fixture.reserved.load(Ordering::Acquire));
    assert_eq!(
        h.service.state().live[&before.id].conversation.view(),
        &before
    );
    let mut text_choice = browser_choice();
    text_choice.capabilities = PageAssistCapabilitiesV1::TextOnly;
    assert_ne!(
        text_choice,
        browser_choice(),
        "reservation choice equality includes native capability"
    );
}

#[tokio::test]
async fn prior_text_only_choice_cannot_enter_browser_admission() {
    let (h, control) = native_harness(Some("unused"), vec![], false, false, false);
    let conversation = h.conversation().await;
    let result = h
        .service
        .ask_native_mode(native_ask(conversation, "Browser unavailable"), true)
        .await;
    assert!(matches!(
        result,
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::UnsupportedInput
        })
    ));
    assert!(control.requests.lock().unwrap().is_empty());
    assert!(control.drafts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn browser_normal_idle_seal_waits_for_dual_join_without_local_interruption() {
    let (h, fixture) = browser_harness(vec![], "Original final answer");
    let (release, hold) = oneshot::channel();
    *fixture.hold_native.lock().unwrap() = Some(hold);
    let (_, mut task) = ask_browser(&h, "Just reply", false).await;
    until(|| fixture.local_settled.load(Ordering::Acquire)).await;
    assert!(!fixture.native_settled.load(Ordering::Acquire));
    assert!(fixture.reserved.load(Ordering::Acquire));
    assert!(fixture
        .timing
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .deadline()
        .is_ok());
    assert!(matches!(futures_poll(&mut task), std::task::Poll::Pending));
    assert_ne!(h.state().await, PageAssistReplyStateV1::Done);
    release.send(()).unwrap();
    task.await;
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
    let PageAssistMessageV1::Reply { fault, .. } = h.reply().await else {
        panic!()
    };
    assert_eq!(fault, None);
}

// Polling once proves the waiter has not completed; it cannot be mistaken for
// a timeout-based physical-settlement claim.
fn futures_poll(task: &mut TurnTask) -> std::task::Poll<()> {
    let waker = std::task::Waker::noop();
    task.as_mut()
        .poll(&mut std::task::Context::from_waker(waker))
}

#[tokio::test]
async fn browser_normal_terminal_wakes_confirmation_but_waits_original_join() {
    let (h, fixture) = browser_harness(
        vec![("act", json!({"steps":[{"verb":"click","ref":"a6"}]}))],
        "Provider finished",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Place order".into(),
            effect: StepEffect::Commit(WorkConfirmCategoryV1::Purchase),
        },
    );
    let (release, hold) = oneshot::channel();
    *fixture.hold_native.lock().unwrap() = Some(hold);
    let (reply, mut task) = ask_browser(&h, "Order", true).await;
    until(|| {
        h.service
            .state()
            .running
            .values()
            .any(|r| r.decision.is_some())
    })
    .await;
    *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
        status: NativeConversationStatus::Completed,
        final_response: Some("Provider finished".into()),
    });
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .close_admission();
    // A click queued immediately after the seal cannot approve the action.
    let _ = h.service.decide(
        &h.conversation().await,
        reply,
        1,
        PageAssistDecisionV1::Approve,
    );
    until(|| fixture.local_settled.load(Ordering::Acquire)).await;
    assert!(h.page.lock().acted.is_empty());
    assert!(!fixture.native_settled.load(Ordering::Acquire));
    assert!(fixture.reserved.load(Ordering::Acquire));
    assert!(matches!(futures_poll(&mut task), std::task::Poll::Pending));
    release.send(()).unwrap();
    task.await;
    assert_local_interruption(&h, "Provider finished").await;
}

#[tokio::test]
async fn browser_normal_terminal_retains_entered_effect_then_original_join() {
    let (h, fixture, release_action, mut task) = held_browser_effect().await;
    let (release_native, hold) = oneshot::channel();
    *fixture.hold_native.lock().unwrap() = Some(hold);
    *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
        status: NativeConversationStatus::Completed,
        final_response: Some("Provider finished".into()),
    });
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .close_admission();
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!h.page.lock().act_returned);
    assert!(!h.page.lock().act_dropped.load(Ordering::Acquire));
    assert!(!fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.reserved.load(Ordering::Acquire));
    release_action.send(()).unwrap();
    until(|| fixture.local_settled.load(Ordering::Acquire)).await;
    assert_eq!(h.page.lock().acted.len(), 1);
    assert!(!fixture.native_settled.load(Ordering::Acquire));
    assert!(matches!(futures_poll(&mut task), std::task::Poll::Pending));
    release_native.send(()).unwrap();
    task.await;
    assert_local_interruption(&h, "Provider finished").await;
}

async fn assert_local_interruption(h: &Harness, expected: &str) {
    let PageAssistMessageV1::Reply {
        state,
        fault,
        blocks,
        ..
    } = h.reply().await
    else {
        panic!()
    };
    assert_eq!(state, PageAssistReplyStateV1::Failed);
    assert_eq!(
        fault,
        Some(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::LocalInterruption
        })
    );
    assert!(matches!(&blocks[0], PageAssistBlockV1::Text { text, .. } if text == expected));
}

#[tokio::test]
async fn browser_unobserved_or_quarantined_completion_never_becomes_normal_done() {
    for settlement in [
        NativeBrowserSettlement::Joined {
            local: NativeBrowserLocalOutcome::ResultUnobserved,
        },
        NativeBrowserSettlement::Quarantined,
    ] {
        let (h, fixture) = browser_harness(vec![], "Known provider answer");
        *fixture.completion_settlement.lock().unwrap() = settlement;
        let (_, task) = ask_browser(&h, "Just reply", false).await;
        task.await;
        assert_local_interruption(&h, "Known provider answer").await;
        assert!(fixture.local_settled.load(Ordering::Acquire));
        let joined = matches!(settlement, NativeBrowserSettlement::Joined { .. });
        assert_eq!(fixture.native_settled.load(Ordering::Acquire), joined);
        assert_eq!(fixture.reserved.load(Ordering::Acquire), !joined);
        if !joined {
            assert!(matches!(
                h.service
                    .ask_native_mode(native_ask(h.conversation().await, "must not resend"), true)
                    .await,
                Err(PageAssistFaultV1::Busy)
            ));
        }
    }
}

#[tokio::test]
async fn browser_no_provider_result_error_still_waits_actual_join_or_retains_quarantine() {
    for settlement in [
        NativeBrowserSettlement::Joined {
            local: NativeBrowserLocalOutcome::Observed,
        },
        NativeBrowserSettlement::Quarantined,
    ] {
        let (h, fixture) = browser_harness(vec![], "");
        *fixture.provider_error.lock().unwrap() = Some(PageAssistNativeFaultV1::OutcomeUnknown);
        *fixture.completion_settlement.lock().unwrap() = settlement;
        let (release, hold) = oneshot::channel();
        *fixture.hold_native.lock().unwrap() = Some(hold);
        let (_, mut task) = ask_browser(&h, "No answer", false).await;
        until(|| fixture.local_settled.load(Ordering::Acquire)).await;
        assert!(matches!(futures_poll(&mut task), std::task::Poll::Pending));
        assert!(fixture.reserved.load(Ordering::Acquire));
        release.send(()).unwrap();
        task.await;
        let PageAssistMessageV1::Reply { state, fault, .. } = h.reply().await else {
            panic!()
        };
        assert_eq!(state, PageAssistReplyStateV1::Failed);
        assert_eq!(
            fault,
            Some(PageAssistFaultV1::NativeConversation {
                fault: PageAssistNativeFaultV1::OutcomeUnknown
            })
        );
        assert_eq!(
            fixture.reserved.load(Ordering::Acquire),
            settlement == NativeBrowserSettlement::Quarantined
        );
    }
}

#[tokio::test]
async fn browser_seal_wakes_actual_takeover_without_running_next_effect() {
    let (h, fixture, release_action, task) = held_browser_effect().await;
    h.service.human_input(TAB);
    release_action.send(()).unwrap();
    until(|| {
        h.service
            .state()
            .running
            .values()
            .any(|r| r.resume.is_some())
    })
    .await;
    *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
        status: NativeConversationStatus::Completed,
        final_response: Some("Provider finished".into()),
    });
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .close_admission();
    task.await;
    assert_eq!(h.page.lock().acted.len(), 1);
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert_local_interruption(&h, "Provider finished").await;
}

#[tokio::test]
async fn browser_quarantined_noncompleted_status_is_never_called_provider_completed() {
    for (status, expected) in [
        (
            NativeConversationStatus::Unknown,
            Some(PageAssistNativeFaultV1::OutcomeUnknown),
        ),
        (
            NativeConversationStatus::HistoryUncertain,
            Some(PageAssistNativeFaultV1::HistoryUncertain),
        ),
        (NativeConversationStatus::Interrupted, None),
    ] {
        let (h, fixture) = browser_harness(vec![], "retained item");
        *fixture.completion_settlement.lock().unwrap() = NativeBrowserSettlement::Quarantined;
        *fixture.sealed_outcome.lock().unwrap() = Some(NativeConversationOutcome {
            status,
            final_response: Some("retained item".into()),
        });
        let (_, task) = ask_browser(&h, "Just reply", false).await;
        task.await;
        let PageAssistMessageV1::Reply { state, fault, .. } = h.reply().await else {
            panic!()
        };
        assert_ne!(state, PageAssistReplyStateV1::Done);
        assert_eq!(
            fault,
            expected.map(|fault| PageAssistFaultV1::NativeConversation { fault })
        );
        assert!(fixture.reserved.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn browser_seal_during_availability_cannot_begin_the_pending_page_read() {
    let (h, fixture) = browser_harness(vec![], "unused");
    let (release, wait) = oneshot::channel();
    *fixture.availability_wait.lock().unwrap() = Some(wait);
    let (_, task) = ask_browser(&h, "Read", true).await;
    until(|| fixture.availability_entered.load(Ordering::Acquire)).await;
    assert_eq!(h.page.lock().reads, 0);
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .close_admission();
    release.send(()).unwrap();
    task.await;
    assert_eq!(h.page.lock().reads, 0);
    assert!(!fixture.provider_started.load(Ordering::Acquire));
    assert!(fixture.local_settled.load(Ordering::Acquire));
}

#[tokio::test]
async fn browser_seal_during_context_read_cannot_begin_following_style_outline() {
    let (h, fixture) = browser_harness(vec![], "unused");
    let (release, wait) = oneshot::channel();
    h.page.lock().read_wait = Some(wait);
    let mut request = native_ask(h.service.current(SPACE, profile()), "Restyle");
    request.suggestion = Some(PageAssistSuggestionV1::Redesign);
    request
        .attachments
        .push(PageAssistAttachV1::Page { tab: TAB.into() });
    let (_, task) = h.service.ask_native_mode(request, true).await.unwrap();
    until(|| h.page.lock().read_entered).await;
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .close_admission();
    release.send(()).unwrap();
    task.await;
    assert!(h.page.lock().read_returned);
    assert_eq!(h.page.lock().outlines, 0);
    assert!(!fixture.provider_started.load(Ordering::Acquire));
    assert!(fixture.local_settled.load(Ordering::Acquire));
}

#[tokio::test]
async fn browser_seal_during_outline_drains_it_without_new_cache_or_style_proposal() {
    let (h, fixture) = browser_harness(
        vec![(
            "restyle",
            json!({
                "page":"a","summary":"Readable","css":"body{background-color:#fafafa!important;color:#111111!important}"
            }),
        )],
        "Provider finished",
    );
    let (release, wait) = oneshot::channel();
    {
        let mut page = h.page.lock();
        page.outline_wait = Some(wait);
        page.outline = Some(Outline {
            nodes: Vec::new(),
            background: Some(Rgb(255, 255, 255)),
            text: Some(Rgb(0, 0, 0)),
            link: None,
            accent: None,
            fonts: vec!["sans-serif".into()],
            dark: false,
        });
    }
    let (_, task) = ask_browser(&h, "Restyle", true).await;
    until(|| h.page.lock().outline_entered).await;
    fixture
        .revoke
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .close_admission();
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!h.page.lock().outline_returned);
    assert!(!h.page.lock().outline_dropped.load(Ordering::Acquire));
    assert!(!fixture.local_settled.load(Ordering::Acquire));
    release.send(()).unwrap();
    task.await;
    assert!(h.page.lock().outline_returned);
    assert!(!h
        .blocks()
        .await
        .iter()
        .any(|block| matches!(block, PageAssistBlockV1::Style { .. })));
    assert!(h
        .service
        .state()
        .live
        .values()
        .flat_map(|live| live.replies.values())
        .all(|data| data.outlines.is_empty() && data.styles.is_empty()));
    assert_local_interruption(&h, "Provider finished").await;
}

#[tokio::test]
async fn browser_sealed_queued_request_never_enters_an_effect() {
    let (h, fixture) = browser_harness(
        vec![("act", json!({"steps":[{"verb":"click","ref":"a6"}]}))],
        "Provider finished",
    );
    h.page.lock().steps.insert(
        6,
        ResolvedStep {
            label: "Expand".into(),
            effect: StepEffect::Read,
        },
    );
    fixture.seal_queued_request.store(true, Ordering::Release);
    let (_, task) = ask_browser(&h, "Expand", true).await;
    task.await;
    assert!(!h.page.lock().act_entered);
    assert!(h.page.lock().acted.is_empty());
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert_local_interruption(&h, "Provider finished").await;
}

#[tokio::test]
async fn public_ask_uses_original_browser_context_tools_and_dual_settlement() {
    let (h, fixture) = browser_harness(
        vec![("read_page", json!({"tab":"t1"}))],
        "Original answer [a1].",
    );
    let words = "  Read the current page\r\nexact words  ";
    let mut request = native_ask(h.service.current(SPACE, profile()), words);
    request
        .attachments
        .push(PageAssistAttachV1::Page { tab: TAB.into() });
    let (_, task) = h.service.ask(request).await.unwrap();
    assert!(fixture.committed.load(Ordering::Acquire));
    task.await;
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
    assert!(fixture.local_settled.load(Ordering::Acquire));
    assert!(fixture.native_settled.load(Ordering::Acquire));
    assert_eq!(fixture.inputs.lock().unwrap().len(), 1);
    let input = fixture.inputs.lock().unwrap()[0].clone();
    assert!(input.contains("<<<PAGE a\nhost: news.ycombinator.com"));
    assert!(input.ends_with(words));
    assert_eq!(fixture.outputs.lock().unwrap().len(), 1);
    assert!(
        matches!(&h.blocks().await[0], PageAssistBlockV1::Text { text, cites } if text.starts_with("Original answer ") && cites.len() == 1)
    );
    assert!(
        h.requests().is_empty(),
        "original native path never falls back to Model"
    );
}

#[tokio::test]
async fn public_ask_refuses_text_only_before_reservation_or_visible_mutation() {
    let (h, fixture) = native_harness(Some("must not run"), vec![], false, false, false);
    let before = h.service.status(SPACE, Some(TAB)).await.conversation;
    let event_count = h.sink.0.lock().unwrap().len();
    let result = h
        .service
        .ask(native_ask(before.id.clone(), "No degraded fallback"))
        .await;
    assert!(matches!(
        result,
        Err(PageAssistFaultV1::NativeConversation {
            fault: PageAssistNativeFaultV1::UnsupportedInput
        })
    ));
    assert!(fixture.drafts.lock().unwrap().is_empty());
    assert!(fixture.requests.lock().unwrap().is_empty());
    assert!(!fixture.reserved.load(Ordering::Acquire));
    assert_eq!(h.page.lock().reads, 0);
    assert_eq!(h.sink.0.lock().unwrap().len(), event_count);
    assert_eq!(
        h.service.state().live[&before.id].conversation.view(),
        &before
    );
}

#[tokio::test]
async fn public_ask_refuses_old_policy_or_revoked_reservation_before_say() {
    for revoked_after_reserve in [false, true] {
        let (h, fixture) = browser_harness(vec![], "must not run");
        if revoked_after_reserve {
            *fixture.validate_failure.lock().unwrap() = Some(PageAssistNativeFaultV1::ScopeChanged);
        } else {
            fixture.policy_mismatch.store(true, Ordering::Release);
        }
        // `status` legitimately reads a Page-capable tab for suggestions.
        // Capture the empty transcript without that unrelated UI observation.
        let conversation = h.service.current(SPACE, profile());
        let before = h.service.state().live[&conversation]
            .conversation
            .view()
            .clone();
        let event_count = h.sink.0.lock().unwrap().len();
        let result = h
            .service
            .ask(native_ask(before.id.clone(), "No policy upgrade"))
            .await;
        let expected = if revoked_after_reserve {
            PageAssistNativeFaultV1::ScopeChanged
        } else {
            PageAssistNativeFaultV1::HistoryDiverged
        };
        assert!(
            matches!(result, Err(PageAssistFaultV1::NativeConversation { fault }) if fault == expected)
        );
        assert!(!fixture.committed.load(Ordering::Acquire));
        assert!(!fixture.reserved.load(Ordering::Acquire));
        assert!(!fixture.provider_started.load(Ordering::Acquire));
        assert_eq!(
            fixture.drafts.lock().unwrap().len(),
            usize::from(revoked_after_reserve)
        );
        assert!(fixture.inputs.lock().unwrap().is_empty());
        assert_eq!(h.page.lock().reads, 0);
        assert_eq!(h.sink.0.lock().unwrap().len(), event_count);
        assert_eq!(
            h.service.state().live[&before.id].conversation.view(),
            &before
        );
    }
}

#[tokio::test]
async fn public_ask_keeps_ordinary_model_browser_path_unchanged() {
    let h = harness(vec![
        tool("list_tabs", json!({})),
        say("Ordinary model answer"),
    ]);
    let request = native_ask(h.service.current(SPACE, profile()), "List my tabs");
    let (_, task) = h.service.ask(request).await.unwrap();
    task.await;
    assert_eq!(h.state().await, PageAssistReplyStateV1::Done);
    assert_eq!(h.requests().len(), 2);
    assert_eq!(h.requests()[0].tools.len(), 9);
    assert!(h.service.state().native_owners.is_empty());
    assert!(
        matches!(&h.blocks().await[0], PageAssistBlockV1::Text { text, .. } if text == "Ordinary model answer")
    );
}

#[path = "native_preparation.rs"]
mod preparation_tests;
