//! Finite synthetic Browser owner controls. No original executable, Page action,
//! authentication, network or caller-provided transcript is admitted here.
use super::*;
use crate::browser_port::*;
use serde_json::json;
use std::{collections::VecDeque, path::PathBuf, sync::Mutex};
#[derive(Clone, Copy)]
pub(crate) enum Scenario {
    TwoTurns,
    ColdResume,
    CancelInput,
    TerminalRevocation,
    LocalLost,
    DuplicateCall,
}
impl Scenario {
    pub(crate) fn args(self) -> &'static [&'static str] {
        match self {
            Self::TwoTurns => &["transport-child", "browser-two-turns"],
            Self::ColdResume => &["transport-child", "browser-cold-resume"],
            Self::CancelInput => &["transport-child", "browser-cancel-input"],
            Self::TerminalRevocation => &["transport-child", "browser-terminal-revocation"],
            Self::LocalLost => &["transport-child", "browser-local-lost"],
            Self::DuplicateCall => &["transport-child", "browser-duplicate-call"],
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "browser-two-turns" => Self::TwoTurns,
            "browser-cold-resume" => Self::ColdResume,
            "browser-cancel-input" => Self::CancelInput,
            "browser-terminal-revocation" => Self::TerminalRevocation,
            "browser-local-lost" => Self::LocalLost,
            "browser-duplicate-call" => Self::DuplicateCall,
            _ => return None,
        })
    }
}
pub(crate) const INSTRUCTIONS: &str =
    "Fixed synthetic Page browser instructions; no host executors.";
pub(crate) const INPUT: &str = "Complete synthetic Page context $context_skill; user request.";
pub(crate) fn manifest() -> Result<BrowserManifest, &'static str> {
    BrowserManifest::from_native_page_v1(
        INSTRUCTIONS.into(),
        crate::dynamic_tools::PageTool::ALL
            .into_iter()
            .map(|tool| BrowserToolDefinition {
                name: tool.name().into(),
                description: "fixed Page fixture".into(),
                schema: json!({"type":"object"}),
            })
            .collect(),
    )
    .map_err(|_| "browser-fixture-manifest")
}
struct State {
    input: Option<BrowserInput>,
    results: VecDeque<BrowserResult>,
    held: Option<BrowserResultKey>,
    completion: Option<BrowserCompletionKey>,
    calls: usize,
    closed: bool,
    released: bool,
    lost: bool,
    hold_effect: bool,
    revocations: Vec<BrowserRevokeReason>,
    marker: PathBuf,
    deadline: Instant,
}
struct Port(Arc<Mutex<State>>);
struct Budget(Arc<Mutex<State>>);
impl BrowserBudget for Budget {
    fn deadline(&self) -> Result<Instant, BrowserPortFault> {
        Ok(self
            .0
            .lock()
            .map_err(|_| BrowserPortFault::Protocol)?
            .deadline)
    }
    fn hard_deadline(&self) -> Result<Instant, BrowserPortFault> {
        self.deadline()
    }
}
impl BrowserPort for Port {
    fn poll_input(&mut self) -> BrowserInputPoll {
        self.0
            .lock()
            .unwrap()
            .input
            .take()
            .map(BrowserInputPoll::Ready)
            .unwrap_or(BrowserInputPoll::Pending)
    }
    fn budget(&self) -> Arc<dyn BrowserBudget> {
        Arc::new(Budget(self.0.clone()))
    }
    fn dispatch(&mut self, call: BrowserDispatch) -> BrowserDispatchOutcome {
        let mut state = self.0.lock().unwrap();
        assert!(!state.closed && state.held.is_none());
        let (ordinal, name, args, key) = call.into_parts();
        assert_eq!(ordinal, 1);
        assert_eq!(name, "read_page");
        assert_eq!(args, json!({"value":"owned"}));
        state.calls += 1;
        if state.hold_effect {
            state.held = Some(key);
        } else {
            state
                .results
                .push_back(key.complete("synthetic readout".into(), true).ok().unwrap());
        }
        std::fs::write(&state.marker, b"entered").unwrap();
        BrowserDispatchOutcome::Accepted
    }
    fn poll_result(&mut self) -> BrowserResultPoll {
        self.0
            .lock()
            .unwrap()
            .results
            .pop_front()
            .map(BrowserResultPoll::Ready)
            .unwrap_or(BrowserResultPoll::Pending)
    }
    fn revoke(&mut self, reason: BrowserRevokeReason) {
        self.0.lock().unwrap().revocations.push(reason);
    }
    fn close_admission(&mut self, key: BrowserCompletionKey) {
        let mut state = self.0.lock().unwrap();
        assert!(!state.closed);
        state.closed = true;
        state.completion = Some(key);
    }
    fn poll_local_settlement(&mut self) -> BrowserLocalSettlement {
        let mut state = self.0.lock().unwrap();
        if state.closed && state.lost {
            return BrowserLocalSettlement::Lost;
        }
        if state.closed && state.released && state.held.is_none() {
            return state
                .completion
                .take()
                .map(|key| BrowserLocalSettlement::Observed(key.observed()))
                .unwrap_or(BrowserLocalSettlement::Pending);
        }
        BrowserLocalSettlement::Pending
    }
}
type FixturePort = (Box<dyn BrowserPort>, Arc<Mutex<State>>);
fn port(
    scenario: Scenario,
    runtime: &RuntimePaths,
    deadline: Instant,
    input: bool,
) -> Result<FixturePort, &'static str> {
    let state = Arc::new(Mutex::new(State {
        input: if input {
            Some(BrowserInput::from_native_page(INPUT.into()).map_err(|_| "browser-input")?)
        } else {
            None
        },
        results: VecDeque::new(),
        held: None,
        completion: None,
        calls: 0,
        closed: false,
        released: !matches!(scenario, Scenario::TerminalRevocation | Scenario::LocalLost),
        lost: matches!(scenario, Scenario::LocalLost),
        hold_effect: matches!(scenario, Scenario::TerminalRevocation | Scenario::LocalLost),
        revocations: Vec::new(),
        marker: runtime.temp().join("browser-effect-entered"),
        deadline,
    }));
    Ok((Box::new(Port(state.clone())), state))
}
fn begin(
    owner: &mut Connection,
    scope: &Scope,
    scenario: Scenario,
    runtime: &RuntimePaths,
    reply: u64,
    previous: Option<u64>,
    deadline: Instant,
) -> Result<Arc<Mutex<State>>, &'static str> {
    let key = ConversationKey::from_native("synthetic-browser-page".into()).ok_or("browser-key")?;
    let proof = owner
        .prepare_browser(
            scope,
            BrowserDraft {
                key,
                new_conversation: previous.is_none(),
                previous_reply: previous,
                model: "synthetic-picker".into(),
                effort: EffortSelection::Default,
                text: String::new(),
            },
            manifest()?,
            &AtomicBool::new(false),
        )
        .map_err(|_| "browser-prepare")?;
    let (port, state) = port(
        scenario,
        runtime,
        deadline,
        !matches!(scenario, Scenario::CancelInput) || reply > 1,
    )?;
    let cancel = Arc::new(AtomicBool::new(
        matches!(scenario, Scenario::CancelInput) && reply == 1,
    ));
    let admission = TextAdmission::from_native(cancel, Arc::new(AtomicU64::new(1)), 1);
    owner
        .start_browser(
            scope,
            proof,
            BrowserCommit {
                reply,
                admission,
                port,
            },
            &AtomicBool::new(false),
        )
        .map_err(|_| "browser-start")?;
    Ok(state)
}
pub(crate) fn qualify() -> Result<(), &'static str> {
    capability_identity()?;
    for scenario in [
        Scenario::TwoTurns,
        Scenario::ColdResume,
        Scenario::CancelInput,
        Scenario::TerminalRevocation,
        Scenario::LocalLost,
        Scenario::DuplicateCall,
    ] {
        run(scenario)?;
    }
    Ok(())
}
fn run(scenario: Scenario) -> Result<(), &'static str> {
    let (session, runtime) = windows::fixtures::browser_session(scenario)?;
    let capability = crate::browser_capability::BrowserCapability::fixture(&session)
        .map_err(|_| "browser-fixture-capability")?;
    let scope =
        Scope::new("synthetic-browser-profile".into(), [71; 16], 1).map_err(|_| "browser-scope")?;
    let mut store = fixtures::MemoryAssociations::default();
    let association_scope = match store.call(durable::Call::Open {
        incarnation: [72; 16],
    }) {
        durable::Reply::Scope(scope) => scope,
        _ => return Err("browser-store-open"),
    };
    if matches!(scenario, Scenario::ColdResume) {
        let policy = browser_api::policy(&manifest()?);
        for call in [
            durable::Call::Bind {
                scope: association_scope,
                policy,
                conversation: "synthetic-browser-page".into(),
                thread_id: "synthetic-owned-thread".into(),
                expected_revision: None,
            },
            durable::Call::Begin {
                scope: association_scope,
                policy,
                conversation: "synthetic-browser-page".into(),
                expected_revision: 1,
                reply: 1,
            },
            durable::Call::Settle {
                scope: association_scope,
                policy,
                conversation: "synthetic-browser-page".into(),
                expected_revision: 2,
                reply: 1,
                turn_id: "prior-turn".into(),
                outcome: durable::Terminal::Completed,
            },
        ] {
            if !matches!(store.call(call), durable::Reply::Association(Some(_))) {
                return Err("browser-store-cold-seed");
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let stop = AtomicBool::new(false);
    let mut owner = Connection::bootstrap_session(
        scope.clone(),
        session,
        runtime.clone(),
        StoreBinding {
            associations: Box::new(store.clone()),
            scope: association_scope,
        },
        ConfigProjection::V164Plus,
        PreparationBudget::ordinary(deadline).into(),
        &stop,
    )
    .map_err(|_| "browser-bootstrap")?;
    owner.browser_capability = Some(capability);
    while owner.catalog().is_none() {
        check(deadline, &stop).map_err(|_| "browser-catalog-timeout")?;
        let events = owner
            .poll(&scope, &stop)
            .map_err(|_| "browser-catalog-poll")?;
        if events
            .into_iter()
            .any(|event| matches!(event.into_event(), Event::Account(AccountState::ChatGpt)))
        {
            owner
                .refresh_catalog(&scope, &stop)
                .map_err(|_| "browser-catalog-read")?;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let first = if matches!(scenario, Scenario::ColdResume) {
        2
    } else {
        1
    };
    let mut state = begin(
        &mut owner,
        &scope,
        scenario,
        &runtime,
        first,
        (first == 2).then_some(1),
        deadline,
    )?;
    let mut reply = first;
    let mut terminals = 0;
    let mut skipped = 0;
    let mut settled = 0;
    let mut known_after_revoke = false;
    let mut failed = false;
    loop {
        check(deadline, &stop).map_err(|_| "browser-fixture-timeout")?;
        let events = if owner.is_closed() {
            owner
                .poll_browser_settlement(&scope)
                .map_err(|_| "browser-local-drain")?
        } else {
            match owner.poll(&scope, &stop) {
                Ok(events) => events,
                Err(fault) => {
                    if !matches!(
                        (scenario, fault),
                        (Scenario::TerminalRevocation, Fault::NotConnected)
                            | (Scenario::DuplicateCall, Fault::Protocol)
                            | (Scenario::LocalLost, Fault::Protocol)
                    ) {
                        return Err("browser-owner-fault");
                    }
                    failed = true;
                    owner
                        .take_failure_events(&scope)
                        .map_err(|_| "browser-failure-events")?
                }
            }
        };
        for event in events {
            match event.into_event() {
                Event::Conversation(conversation::Event::Terminal(result))
                    if result.outcome == conversation::Outcome::Completed =>
                {
                    terminals += 1;
                    known_after_revoke = true;
                }
                Event::NeverStarted { reply: 1 } => skipped += 1,
                Event::BrowserSettled {
                    reply: observed, ..
                } => {
                    if observed != reply {
                        return Err("browser-settled-reply");
                    }
                    if matches!(scenario, Scenario::CancelInput) && reply == 1 && skipped != 1 {
                        return Err("browser-skip-before-dual-settlement");
                    }
                    settled += 1;
                }
                _ => (),
            }
        }
        if matches!(scenario, Scenario::TerminalRevocation)
            && failed
            && !state.lock().map_err(|_| "browser-state")?.released
        {
            let mut current = state.lock().map_err(|_| "browser-state")?;
            if !known_after_revoke
                || current.calls != 1
                || current.revocations.last() != Some(&BrowserRevokeReason::ScopeChanged)
                || settled != 0
            {
                return Err("browser-terminal-revocation-facts");
            }
            // Actual fixed effect return, not channel drop, releases local custody.
            current.held.take();
            current.released = true;
        }
        if matches!(scenario, Scenario::LocalLost) && owner.browser_local_receipt_lost() {
            if settled != 0 {
                return Err("browser-lost-settled");
            }
            owner
                .retire_browser(&scope, BrowserRevokeReason::ConnectionLost)
                .map_err(|_| "browser-lost-retire")?;
            let quarantine = owner
                .quarantine_browser()
                .map_err(|_| "browser-quarantine")?;
            drop(quarantine);
            drop(state);
            Arc::try_unwrap(runtime)
                .map_err(|_| "browser-runtime-custody")?
                .close()?;
            println!("codex-browser-local-receipt-loss-quarantined: native-code=0");
            return Ok(());
        }
        if settled > 0 {
            let next = matches!(scenario, Scenario::TwoTurns | Scenario::CancelInput) && reply == 1;
            if next {
                if matches!(scenario, Scenario::CancelInput) && skipped != 1 {
                    return Err("browser-input-cancel-not-skipped");
                }
                state = begin(&mut owner, &scope, scenario, &runtime, 2, Some(1), deadline)?;
                reply = 2;
                settled = 0;
            } else {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let expected = match scenario {
        Scenario::TwoTurns => 2,
        Scenario::DuplicateCall => 0,
        _ => 1,
    };
    if terminals != expected
        || (matches!(scenario, Scenario::DuplicateCall)
            && state.lock().map_err(|_| "browser-state")?.calls != 0)
    {
        return Err("browser-final-facts");
    }
    if !owner.is_closed() {
        owner.close().map_err(|_| "browser-owner-close")?;
    } else {
        drop(owner);
    }
    drop(state);
    Arc::try_unwrap(runtime)
        .map_err(|_| "browser-runtime-custody")?
        .close()?;
    println!(
        "{}: native-code=0",
        match scenario {
            Scenario::TwoTurns => "codex-browser-two-turns-same-thread",
            Scenario::ColdResume => "codex-browser-cold-store-same-thread",
            Scenario::CancelInput => "codex-browser-input-cancel-skip-next-turn",
            Scenario::TerminalRevocation => "codex-browser-terminal-revocation-local-drain",
            Scenario::DuplicateCall => "codex-browser-duplicate-request-no-effect",
            Scenario::LocalLost => unreachable!(),
        }
    );
    Ok(())
}

fn capability_identity() -> Result<(), &'static str> {
    let (first, first_root) = windows::fixtures::browser_session(Scenario::DuplicateCall)?;
    let receipt = crate::browser_capability::BrowserCapability::fixture(&first)
        .map_err(|_| "browser-fixture-receipt")?;
    if !receipt.qualifies(&first) {
        return Err("browser-fixture-own-session");
    }
    let (second, second_root) = windows::fixtures::browser_session(Scenario::DuplicateCall)?;
    if receipt.qualifies(&second) {
        return Err("browser-fixture-foreign-session");
    }
    let (ordinary, ordinary_root) =
        windows::fixtures::owner_session(fixture_peer::Scenario::TwoTurns)?;
    if crate::browser_capability::BrowserCapability::fixture(&ordinary).is_ok()
        || receipt.qualifies(&ordinary)
    {
        return Err("browser-fixture-wrong-mode");
    }
    // No initialize/account/thread/model frame was sent. Drop still owns each
    // synthetic process/job/writer to factual termination before cleanup.
    drop(first);
    drop(second);
    drop(ordinary);
    drop(receipt);
    for runtime in [first_root, second_root, ordinary_root] {
        Arc::try_unwrap(runtime)
            .map_err(|_| "browser-identity-runtime")?
            .close()?;
    }
    println!("codex-browser-fixture-session-mode-bound: native-code=0");
    Ok(())
}
