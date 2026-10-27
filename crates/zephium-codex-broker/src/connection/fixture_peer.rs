//! Fixed own-helper conversation peer. No external network, credentials, browser,
//! model request or caller-defined transcript. Parent invokes separate typed mode.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
const THREAD: &str = "synthetic-owned-thread";
const MODEL: &str = "synthetic-owned-model";
const LIMIT: u64 = 1024 * 1024;
#[derive(Clone, Copy)]
pub(crate) enum Scenario {
    TwoTurns,
    QuotaExpired,
    QuotaMalformed,
    QuotaRefused,
    QuotaReplacement,
    QuotaCoalesced,
    ColdRestore,
    Cancel,
    Invalidate,
    CancelBeforeTurn,
    RevisionBeforeWrite,
    LostSettleAck,
    LostStartAck,
    CancelBeforeActor,
    CancelDuringPreparation,
    CancelBeforeWrite,
    LostBindAck,
    LostBeginAck,
    LostSkipAck,
    LostPreparationAck,
}
impl Scenario {
    pub(crate) fn three_turns(self) -> bool {
        matches!(
            self,
            Self::TwoTurns | Self::QuotaExpired | Self::QuotaMalformed | Self::QuotaRefused
        )
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "owner-two-turns" => Some(Self::TwoTurns),
            "quota-expired" => Some(Self::QuotaExpired),
            "quota-malformed" => Some(Self::QuotaMalformed),
            "quota-refused" => Some(Self::QuotaRefused),
            "quota-replacement" => Some(Self::QuotaReplacement),
            "quota-coalesced" => Some(Self::QuotaCoalesced),
            "owner-cold-restore" => Some(Self::ColdRestore),
            "owner-cancel" => Some(Self::Cancel),
            "owner-invalidate" => Some(Self::Invalidate),
            "owner-cancel-before-turn" => Some(Self::CancelBeforeTurn),
            "owner-revision-before-write" => Some(Self::RevisionBeforeWrite),
            "owner-lost-settle-ack" => Some(Self::LostSettleAck),
            "owner-lost-start-ack" => Some(Self::LostStartAck),
            "owner-cancel-before-actor" => Some(Self::CancelBeforeActor),
            "owner-cancel-during-preparation" => Some(Self::CancelDuringPreparation),
            "owner-cancel-before-write" => Some(Self::CancelBeforeWrite),
            "owner-lost-bind-ack" => Some(Self::LostBindAck),
            "owner-lost-begin-ack" => Some(Self::LostBeginAck),
            "owner-lost-skip-ack" => Some(Self::LostSkipAck),
            "owner-lost-preparation-ack" => Some(Self::LostPreparationAck),
            _ => None,
        }
    }
}
struct Peer<R, W> {
    input: R,
    output: W,
    last: u64,
    had_first_turn: bool,
    withhold_open: bool,
    default_choices: bool,
    pending_quota: Option<u64>,
    malformed_quota: bool,
    refused_quota: bool,
    home: String,
    cwd: String,
}
impl<R: BufRead, W: Write> Peer<R, W> {
    fn read(&mut self) -> Result<Option<Value>, &'static str> {
        let mut bytes = Vec::new();
        (&mut self.input)
            .take(LIMIT + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "transcript-read")?;
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() > LIMIT as usize || bytes.pop() != Some(b'\n') {
            return Err("transcript-frame");
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "transcript-json")
    }
    fn request(&mut self, method: &str) -> Result<(u64, Value), &'static str> {
        let value = self.read()?.ok_or("transcript-eof")?;
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("transcript-id")?;
        if id <= self.last || value.get("method").and_then(Value::as_str) != Some(method) {
            return Err("transcript-order");
        }
        self.last = id;
        Ok((id, value))
    }
    fn expect(&mut self, mut expected: Value) -> Result<u64, &'static str> {
        let method = expected["method"]
            .as_str()
            .ok_or("transcript-expected")?
            .to_owned();
        let (id, actual) = self.request(&method)?;
        expected["id"] = json!(id);
        if actual != expected {
            return Err("transcript-request");
        }
        Ok(id)
    }
    fn send(&mut self, value: Value) -> Result<(), &'static str> {
        serde_json::to_writer(&mut self.output, &value).map_err(|_| "transcript-write")?;
        self.output
            .write_all(b"\n")
            .map_err(|_| "transcript-write")?;
        self.output.flush().map_err(|_| "transcript-flush")
    }
    fn reply(&mut self, id: u64, result: Value) -> Result<(), &'static str> {
        self.send(json!({"id":id,"result":result}))
    }
    fn note(&mut self, method: &str, params: Value) -> Result<(), &'static str> {
        self.send(json!({"method":method,"params":params,"emittedAtMs":1}))
    }
    fn bootstrap(&mut self) -> Result<(), &'static str> {
        self.bootstrap_with_catalog(true)
    }
    fn bootstrap_with_catalog(&mut self, catalog: bool) -> Result<(), &'static str> {
        self.bootstrap_account(catalog, true)
    }
    fn bootstrap_account(&mut self, catalog: bool, chatgpt: bool) -> Result<(), &'static str> {
        let expected: Value =
            serde_json::from_slice(&crate::wire::initialize()).map_err(|_| "transcript-init")?;
        let id = self.expect(expected)?;
        self.reply(id,json!({"codexHome":self.home,"platformFamily":"windows","platformOs":"windows","userAgent":"synthetic-original-contract"}))?;
        if self.read()? != Some(json!({"method":"initialized"})) {
            return Err("transcript-initialized");
        }
        // Reproduces the independently identified legitimate retained-home race.
        self.note("account/updated", json!({"authMode":"chatgpt"}))?;
        self.note(
            "warning",
            json!({"message":"synthetic inert startup warning"}),
        )?;
        self.note("skills/changed", json!({}))?;
        self.note("remoteControl/status/changed",json!({"status":"disabled","serverName":"synthetic","installationId":"synthetic","environmentId":null}))?;
        let id = self.expect(
            json!({"method":"config/read","params":{"includeLayers":true,"cwd":self.cwd}}),
        )?;
        self.note("account/updated", json!({"authMode":"chatgpt"}))?;
        self.reply(
            id,
            crate::config::synthetic_readback(&format!("{}\\config.toml", self.home)),
        )?;
        let id = self.expect(json!({"method":"configRequirements/read"}))?;
        self.reply(id, crate::config::synthetic_requirements())?;
        let id = self
            .expect(crate::compatibility::feature_request(1).map_err(|_| "transcript-features")?)?;
        self.reply(id, crate::config::synthetic_features())?;
        let id = self.expect(json!({"method":"account/read","params":{"refreshToken":false}}))?;
        self.reply(
            id,
            json!({"account":if chatgpt { json!({"type":"chatgpt"}) } else { Value::Null },"requiresOpenaiAuth":true}),
        )?;
        if !chatgpt {
            return Ok(());
        }
        let quota_id = self.expect(json!({"method":"account/rateLimits/read","params":{"supportsLunaReserve":false,"excludeResetCreditDetails":true}}))?;
        if self.default_choices || !catalog {
            // The actual turn must remain admissible while this unrelated
            // observational read still awaits its reply.
            self.pending_quota = Some(quota_id);
        } else {
            self.reply(quota_id, Self::quota_reading())?;
        }
        if !catalog {
            return Ok(());
        }
        let id = self.expect(crate::wire::model_list(0, None))?;
        self.reply(id,json!({"data":[{"id":"synthetic-picker","model":MODEL,"displayName":"Synthetic","description":"owned fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"fixture"},{"reasoningEffort":"none","description":"explicit none"}],"defaultReasoningEffort":"high","isDefault":true,"inputModalities":["text"]}],"nextCursor":null}))
    }
    fn quota_reading() -> Value {
        json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":10,"windowDurationMins":300,"resetsAt":2000000000},"secondary":null,"planType":"plus"}})
    }
    fn opened(&mut self, resume: bool, text: &str, defer: bool) -> Result<(), &'static str> {
        let method = if resume {
            "thread/resume"
        } else {
            "thread/start"
        };
        let (id, actual) = self.request(method)?;
        let effort = self.effort(text);
        let text = crate::text_policy::prepare_text(text.into()).map_err(|_| "transcript-text")?;
        let mut config = text.fixed_config();
        if let Some(effort) = effort {
            config["model_reasoning_effort"] = json!(effort);
        }
        let mut params = json!({"model":MODEL,"modelProvider":"openai","cwd":self.cwd,"approvalPolicy":"never","sandbox":"read-only","serviceTier":null,"config":config});
        if resume {
            params["threadId"] = json!(THREAD);
            params["excludeTurns"] = json!(true);
        } else {
            params["ephemeral"] = json!(false);
            params["environments"] = json!([]);
            params["dynamicTools"] = json!([]);
            params["selectedCapabilityRoots"] = json!([]);
            params["allowProviderModelFallback"] = json!(false);
        }
        if actual != json!({"id":id,"method":method,"params":params}) {
            return Err("transcript-open-policy");
        }
        if self.withhold_open {
            return Ok(());
        }
        if defer {
            return self.send(json!({"id":id,"error":{"code":-32600,"message":format!("thread {THREAD} is closing; retry thread/resume after the thread is closed")}}));
        }
        self.note("thread/started", json!({"thread":{"id":THREAD}}))?;
        self.note(
            "thread/name/updated",
            json!({"threadId":THREAD,"threadName":"synthetic title"}),
        )?;
        self.reply(id,json!({"thread":{"id":THREAD,"ephemeral":false,"status":{"type":"idle"},"environments":[]},"model":MODEL,"modelProvider":"openai","reasoningEffort":effort,"cwd":self.cwd,"approvalPolicy":"never","sandbox":{"type":"readOnly","networkAccess":false}}))
    }
    fn effort(&self, text: &str) -> Option<&'static str> {
        if !self.default_choices {
            Some("high")
        } else if text == "synthetic second $skill_two" {
            Some("none")
        } else {
            None
        }
    }
    fn start(&mut self, text: &str, turn: &str) -> Result<(), &'static str> {
        let mut params = json!({"threadId":THREAD,"input":[{"type":"text","text":text,"text_elements":[]}],"model":MODEL,"environments":[]});
        if let Some(effort) = self.effort(text) {
            params["effort"] = json!(effort);
        }
        let id = self.expect(json!({"method":"turn/start","params":params}))?;
        if turn == "turn-two" && self.had_first_turn {
            self.usage_tail("turn-one")?;
        }
        if turn == "turn-one" {
            self.had_first_turn = true;
        }
        if let Some(quota_id) = self.pending_quota.take() {
            let mut reading = Self::quota_reading();
            if self.malformed_quota {
                reading["rateLimits"]["primary"]["usedPercent"] = json!(101);
            }
            if self.refused_quota {
                self.send(json!({"id":quota_id,"error":{"code":-32000,"message":"synthetic quota refusal"}}))?;
            } else {
                self.reply(quota_id, reading)?;
            }
        }
        // Informational producer can race ahead of the RPC response. It must
        // be correlated later without creating/finishing a turn by itself.
        self.advisories(turn)?;
        self.reply(
            id,
            json!({"turn":{"id":turn,"status":"inProgress","error":null}}),
        )?;
        self.note(
            "thread/status/changed",
            json!({"threadId":THREAD,"status":{"type":"active","activeFlags":[]}}),
        )?;
        self.note(
            "account/rateLimits/updated",
            json!({"rateLimits":{"primary":null}}),
        )
    }
    fn advisories(&mut self, turn: &str) -> Result<(), &'static str> {
        self.note(
            "model/verification",
            json!({"threadId":THREAD,"turnId":turn,"verifications":[]}),
        )?;
        self.note(
            "turn/moderationMetadata",
            json!({"threadId":THREAD,"turnId":turn,"metadata":null}),
        )?;
        self.note("model/safetyBuffering/updated", json!({"threadId":THREAD,"turnId":turn,"model":MODEL,"useCases":[],"reasons":[],"showBufferingUi":false,"fasterModel":null}))?;
        self.note("modelProvider/authRecoveryStarted", json!({"threadId":THREAD,"turnId":turn,"provider":"openai","message":"synthetic recovery"}))?;
        self.note("modelProvider/authRecoveryCompleted", json!({"threadId":THREAD,"turnId":turn,"provider":"openai","message":"synthetic recovered"}))?;
        self.note(
            "guardianWarning",
            json!({"threadId":THREAD,"message":"synthetic inert warning"}),
        )?;
        self.note("thread/queue/changed", json!({"threadId":THREAD}))
    }
    fn usage_tail(&mut self, turn: &str) -> Result<(), &'static str> {
        let usage = json!({"totalTokens":2,"inputTokens":1,"cachedInputTokens":0,"cacheWriteInputTokens":0,"outputTokens":1,"reasoningOutputTokens":0});
        self.note("thread/tokenUsage/updated", json!({"threadId":THREAD,"turnId":turn,"tokenUsage":{"total":usage,"last":usage,"modelContextWindow":null}}))
    }
    fn terminal(&mut self, turn: &str, answer: &str, status: &str) -> Result<(), &'static str> {
        let item = json!({"id":format!("item-{turn}"),"type":"agentMessage","text":answer,"phase":"final_answer"});
        self.note(
            "item/agentMessage/delta",
            json!({"threadId":THREAD,"turnId":turn,"itemId":format!("item-{turn}"),"delta":answer}),
        )?;
        self.note(
            "item/completed",
            json!({"threadId":THREAD,"turnId":turn,"item":item}),
        )?;
        self.note("turn/completed",json!({"threadId":THREAD,"turn":{"id":turn,"status":status,"error":null,"itemsView":"full","items":[item]}}))
    }
    fn close_without_turn(&mut self) -> Result<(), &'static str> {
        // Successful fixture completion proves the next child-observed frame
        // was unsubscribe, never a model turn carrying the cancelled text.
        let id =
            self.expect(json!({"method":"thread/unsubscribe","params":{"threadId":THREAD}}))?;
        self.note("thread/closed", json!({"threadId":THREAD}))?;
        self.reply(id, json!({"status":"unsubscribed"}))
    }
    fn complete(&mut self, turn: &str, answer: &str, status: &str) -> Result<(), &'static str> {
        self.terminal(turn, answer, status)?;
        let id =
            self.expect(json!({"method":"thread/unsubscribe","params":{"threadId":THREAD}}))?;
        // Real matching close and ACK are independently necessary; test opposite order.
        self.note("thread/closed", json!({"threadId":THREAD}))?;
        self.reply(id, json!({"status":"unsubscribed"}))?;
        self.advisories(turn)?;
        self.usage_tail(turn)?;
        self.note("thread/readState/changed", json!({"threadId":THREAD,"readState":{"revision":"synthetic","firstUnread":{"type":"turn","turnId":turn}}}))
    }
}
pub(crate) fn child(scenario: Scenario) -> Result<(), &'static str> {
    let home = std::env::var("CODEX_HOME").map_err(|_| "transcript-home")?;
    let cwd = std::env::current_dir()
        .map_err(|_| "transcript-cwd")?
        .to_str()
        .ok_or("transcript-cwd")?
        .to_owned();
    let mut peer = Peer {
        input: std::io::BufReader::new(std::io::stdin()),
        output: std::io::stdout(),
        last: 0,
        had_first_turn: matches!(scenario, Scenario::ColdRestore),
        withhold_open: matches!(scenario, Scenario::LostPreparationAck),
        default_choices: scenario.three_turns(),
        pending_quota: None,
        malformed_quota: matches!(scenario, Scenario::QuotaMalformed),
        refused_quota: matches!(scenario, Scenario::QuotaRefused),
        home,
        cwd,
    };
    if matches!(
        scenario,
        Scenario::QuotaReplacement | Scenario::QuotaCoalesced
    ) {
        peer.bootstrap_with_catalog(false)?;
        let old = peer.pending_quota.take().ok_or("quota-old-request")?;
        if matches!(scenario, Scenario::QuotaCoalesced) {
            let temp =
                std::path::PathBuf::from(std::env::var_os("TEMP").ok_or("quota-helper-temp")?);
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !temp.join("quota-batch-go").exists() {
                if std::time::Instant::now() >= until {
                    return Err("quota-helper-batch-go");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let mut batch = serde_json::to_vec(&json!({"id":old,"result":Peer::<std::io::BufReader<std::io::Stdin>,std::io::Stdout>::quota_reading()})).map_err(|_| "quota-helper-batch")?;
            batch.push(b'\n');
            serde_json::to_writer(&mut batch, &json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":null}}})).map_err(|_| "quota-helper-batch")?;
            batch.push(b'\n');
            serde_json::to_writer(
                &mut batch,
                &json!({"method":"account/updated","params":{"authMode":"chatgpt"}}),
            )
            .map_err(|_| "quota-helper-batch")?;
            batch.push(b'\n');
            peer.output
                .write_all(&batch)
                .map_err(|_| "quota-helper-batch-write")?;
            peer.output
                .flush()
                .map_err(|_| "quota-helper-batch-write")?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(temp.join("quota-batch-ready"))
                .map_err(|_| "quota-helper-batch-ready")?;
        } else {
            peer.note("account/updated", json!({"authMode":"chatgpt"}))?;
        }
        let read = peer.expect(json!({"method":"account/read","params":{"refreshToken":false}}))?;
        peer.reply(
            read,
            json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true}),
        )?;
        let mut reading =
            Peer::<std::io::BufReader<std::io::Stdin>, std::io::Stdout>::quota_reading();
        reading["rateLimits"]["primary"]["usedPercent"] = json!(99);
        if !matches!(scenario, Scenario::QuotaCoalesced) {
            peer.reply(old, reading)?;
        }
        if peer.read()?.is_some() {
            return Err("quota-replacement-unexpected-request");
        }
        return Ok(());
    }
    peer.bootstrap()?;
    if matches!(scenario, Scenario::ColdRestore) {
        peer.opened(true, "synthetic second $skill_two", false)?;
        peer.start("synthetic second $skill_two", "turn-two")?;
        peer.complete("turn-two", "synthetic second answer", "completed")?;
        if peer.read()?.is_some() {
            return Err("transcript-unexpected-replay");
        }
        return Ok(());
    }
    if matches!(scenario, Scenario::CancelBeforeActor) {
        peer.opened(false, "synthetic second $skill_two", false)?;
        peer.start("synthetic second $skill_two", "turn-two")?;
        peer.complete("turn-two", "synthetic second answer", "completed")?;
        if peer.read()?.is_some() {
            return Err("transcript-replayed-unsent");
        }
        return Ok(());
    }
    peer.opened(false, "synthetic first $skill_one", false)?;
    if matches!(scenario, Scenario::LostPreparationAck) {
        return Ok(());
    }
    if matches!(
        scenario,
        Scenario::CancelBeforeTurn
            | Scenario::CancelDuringPreparation
            | Scenario::CancelBeforeWrite
            | Scenario::LostSkipAck
    ) {
        peer.close_without_turn()?;
        if !matches!(scenario, Scenario::LostSkipAck) {
            peer.opened(true, "synthetic second $skill_two", false)?;
            peer.start("synthetic second $skill_two", "turn-two")?;
            peer.complete("turn-two", "synthetic second answer", "completed")?;
        }
        if peer.read()?.is_some() {
            return Err("transcript-replayed-unsent");
        }
        return Ok(());
    }
    if matches!(
        scenario,
        Scenario::RevisionBeforeWrite | Scenario::LostBindAck | Scenario::LostBeginAck
    ) {
        if peer.read()?.is_some() {
            return Err("transcript-retired-turn");
        }
        return Ok(());
    }
    if matches!(scenario, Scenario::LostStartAck) {
        let id=peer.expect(json!({"method":"turn/start","params":{"threadId":THREAD,"input":[{"type":"text","text":"synthetic first $skill_one","text_elements":[]}],"model":MODEL,"effort":"high","environments":[]}}))?;
        peer.terminal("turn-one", "synthetic preserved answer", "completed")?;
        peer.reply(
            id,
            json!({"turn":{"id":"turn-one","status":"inProgress","error":null}}),
        )?;
        if peer.read()?.is_some() {
            return Err("transcript-unexpected-replay");
        }
        return Ok(());
    }
    peer.start("synthetic first $skill_one", "turn-one")?;
    match scenario {
        Scenario::TwoTurns
        | Scenario::QuotaExpired
        | Scenario::QuotaMalformed
        | Scenario::QuotaRefused => {
            peer.note("error",json!({"threadId":THREAD,"turnId":"turn-one","error":{"message":"synthetic ignored text"},"willRetry":true}))?;
            peer.complete("turn-one", "synthetic first answer", "completed")?;
            peer.opened(true, "synthetic second $skill_two", true)?;
            peer.opened(true, "synthetic second $skill_two", false)?;
            peer.start("synthetic second $skill_two", "turn-two")?;
            peer.complete("turn-two", "synthetic second answer", "completed")?;
            peer.opened(true, "synthetic third $skill_three", false)?;
            peer.start("synthetic third $skill_three", "turn-three")?;
            peer.complete("turn-three", "synthetic third answer", "completed")?;
        }
        Scenario::Cancel => {
            let id = peer.expect(
                json!({"method":"turn/interrupt","params":{"threadId":THREAD,"turnId":"turn-one"}}),
            )?;
            peer.reply(id, json!({}))?;
            peer.complete("turn-one", "synthetic cancelled output", "interrupted")?;
        }
        Scenario::Invalidate => {
            peer.note("account/updated", json!({"authMode":"chatgpt"}))?;
        }
        Scenario::LostSettleAck => {
            peer.terminal("turn-one", "synthetic preserved answer", "completed")?;
        }
        Scenario::QuotaReplacement
        | Scenario::QuotaCoalesced
        | Scenario::LostStartAck
        | Scenario::ColdRestore
        | Scenario::CancelBeforeTurn
        | Scenario::RevisionBeforeWrite
        | Scenario::CancelBeforeActor
        | Scenario::CancelDuringPreparation
        | Scenario::CancelBeforeWrite
        | Scenario::LostBindAck
        | Scenario::LostBeginAck
        | Scenario::LostSkipAck
        | Scenario::LostPreparationAck => {
            unreachable!()
        }
    }
    if peer.read()?.is_some() {
        return Err("transcript-unexpected-replay");
    }
    Ok(())
}

/// Fixed local-only removal peer; uses no original login or credential store.
pub(crate) fn removal_child(
    scenario: super::removal_fixtures::Scenario,
) -> Result<(), &'static str> {
    use super::removal_fixtures::Scenario;
    let mut peer = Peer {
        input: std::io::BufReader::new(std::io::stdin()),
        output: std::io::stdout(),
        last: 0,
        had_first_turn: false,
        withhold_open: false,
        default_choices: false,
        pending_quota: None,
        malformed_quota: false,
        refused_quota: false,
        home: std::env::var("CODEX_HOME").map_err(|_| "removal-helper-home")?,
        cwd: std::env::current_dir()
            .map_err(|_| "removal-helper-cwd")?
            .to_str()
            .ok_or("removal-helper-cwd")?
            .to_owned(),
    };
    peer.bootstrap_account(
        false,
        matches!(scenario, Scenario::PendingQuota | Scenario::PendingCatalog),
    )?;
    if matches!(scenario, Scenario::PendingCatalog) {
        let catalog = peer.expect(crate::wire::model_list(0, None))?;
        let quota = peer
            .pending_quota
            .take()
            .ok_or("removal-helper-catalog-quota")?;
        peer.reply(
            quota,
            Peer::<std::io::BufReader<std::io::Stdin>, std::io::Stdout>::quota_reading(),
        )?;
        peer.reply(
            catalog,
            json!({"data":[],"nextCursor":"must-not-be-requested"}),
        )?;
    }
    let logout = peer.expect(json!({"method":"account/logout"}))?;
    peer.reply(logout, json!({}))?;
    if matches!(scenario, Scenario::MissingNotification) {
        // An ACK alone must not cause a fresh read or any follow-up mutation.
        if peer.read()?.is_some() {
            return Err("removal-helper-ack-only-read");
        }
        return Ok(());
    }
    peer.note("account/updated", json!({"authMode":null}))?;
    let read = peer.expect(json!({"method":"account/read","params":{"refreshToken":false}}))?;
    if matches!(scenario, Scenario::InvalidatedRead) {
        peer.note("account/updated", json!({"authMode":"chatgpt"}))?;
    }
    peer.reply(read, json!({"account":null,"requiresOpenaiAuth":true}))?;
    if matches!(scenario, Scenario::PendingQuota) {
        let temp = std::path::PathBuf::from(std::env::var_os("TEMP").ok_or("removal-helper-temp")?);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !temp.join("removal-quota-drain-observed").exists() {
            if std::time::Instant::now() >= until {
                return Err("removal-helper-quota-barrier");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let quota = peer
            .pending_quota
            .take()
            .ok_or("removal-helper-owned-quota")?;
        peer.reply(
            quota,
            Peer::<std::io::BufReader<std::io::Stdin>, std::io::Stdout>::quota_reading(),
        )?;
    }
    if peer.read()?.is_some() {
        return Err("removal-helper-unexpected-request");
    }
    match scenario {
        Scenario::LateInvalidation => peer.note("account/updated", json!({"authMode":"chatgpt"})),
        Scenario::NonzeroExit => std::process::exit(7),
        Scenario::CancelDuringFinish => {
            let temp =
                std::path::PathBuf::from(std::env::var_os("TEMP").ok_or("removal-helper-temp")?);
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(temp.join("removal-finish-started"))
                .map_err(|_| "removal-helper-finish-barrier")?;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !temp.join("removal-finish-cancelled").exists() {
                if std::time::Instant::now() >= until {
                    return Err("removal-helper-cancel-barrier");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Fixed Browser transcript peer; all offered definitions and instructions are
/// the same native fixture manifest. No arbitrary request/response script.
pub(crate) fn browser_child(
    scenario: super::browser_fixtures::Scenario,
) -> Result<(), &'static str> {
    use super::browser_fixtures::{manifest, Scenario as B, INPUT, INSTRUCTIONS};
    let home = std::env::var("CODEX_HOME").map_err(|_| "browser-peer-home")?;
    let cwd = std::env::current_dir()
        .map_err(|_| "browser-peer-cwd")?
        .to_str()
        .ok_or("browser-peer-cwd")?
        .to_owned();
    let mut peer = Peer {
        input: std::io::BufReader::new(std::io::stdin()),
        output: std::io::stdout(),
        last: 0,
        had_first_turn: false,
        withhold_open: false,
        default_choices: false,
        pending_quota: None,
        malformed_quota: false,
        refused_quota: false,
        home,
        cwd,
    };
    peer.bootstrap()?;
    let rounds = if matches!(scenario, B::TwoTurns) {
        2
    } else {
        1
    };
    for round in 0..rounds {
        let resume = round > 0 || matches!(scenario, B::ColdResume);
        let method = if resume {
            "thread/resume"
        } else {
            "thread/start"
        };
        let (id, actual) = peer.request(method)?;
        let text = crate::text_policy::prepare_browser_text(INPUT.into())
            .map_err(|_| "browser-peer-input")?
            .into_validated_text();
        let config = crate::compatibility::ConfigProjection::V164Plus.fixed_config(&text);
        let mut params = json!({"model":MODEL,"modelProvider":"openai","cwd":peer.cwd,"approvalPolicy":"never","sandbox":"read-only","serviceTier":null,"config":config,"developerInstructions":INSTRUCTIONS});
        if resume {
            params["threadId"] = json!(THREAD);
            params["excludeTurns"] = json!(true);
        } else {
            params["environments"] = json!([]);
            params["ephemeral"] = json!(false);
            params["allowProviderModelFallback"] = json!(false);
            params["dynamicTools"] = manifest()?.tools().wire().clone();
            params["selectedCapabilityRoots"] = json!([]);
        }
        if actual != json!({"id":id,"method":method,"params":params}) {
            return Err("browser-peer-open-policy");
        }
        peer.reply(id,json!({"thread":{"id":THREAD,"ephemeral":false,"status":{"type":"idle"},"environments":[]},"model":MODEL,"modelProvider":"openai","reasoningEffort":null,"cwd":peer.cwd,"approvalPolicy":"never","sandbox":{"type":"readOnly","networkAccess":false}}))?;
        let id=peer.expect(json!({"method":"turn/start","params":{"threadId":THREAD,"input":[{"type":"text","text":INPUT,"text_elements":[]}],"model":MODEL,"environments":[]}}))?;
        let turn = if round == 0 {
            "browser-turn-one"
        } else {
            "browser-turn-two"
        };
        let rpc = if round == 0 {
            json!(-7)
        } else {
            json!("server-string-id")
        };
        let call = format!("browser-call-{round}");
        let request = json!({"id":rpc,"method":"item/tool/call","params":{"threadId":THREAD,"turnId":turn,"callId":call,"namespace":null,"tool":"read_page","arguments":{"value":"owned"}}});
        let item = json!({"type":"dynamicToolCall","id":call,"namespace":null,"tool":"read_page","arguments":{"value":"owned"},"status":"inProgress","contentItems":null,"success":null,"durationMs":null});
        peer.note(
            "item/started",
            json!({"threadId":THREAD,"turnId":turn,"item":item,"startedAtMs":1}),
        )?;
        peer.send(request.clone())?;
        if matches!(scenario, B::DuplicateCall) {
            peer.send(request)?;
        }
        peer.reply(
            id,
            json!({"turn":{"id":turn,"status":"inProgress","error":null}}),
        )?;
        if matches!(scenario, B::DuplicateCall) {
            if peer.read()?.is_some() {
                return Err("browser-duplicate-produced-response");
            }
            return Ok(());
        }
        if matches!(scenario, B::TerminalRevocation | B::LocalLost) {
            let marker =
                std::path::PathBuf::from(std::env::var_os("TEMP").ok_or("browser-peer-temp")?)
                    .join("browser-effect-entered");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !marker.is_file() {
                if std::time::Instant::now() >= deadline {
                    return Err("browser-effect-marker-timeout");
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            if matches!(scenario, B::TerminalRevocation) {
                let answer = "authentic browser final";
                let final_item = json!({"id":"browser-final","type":"agentMessage","text":answer,"phase":"final_answer"});
                let values = [
                    json!({"method":"item/agentMessage/delta","params":{"threadId":THREAD,"turnId":turn,"itemId":"browser-final","delta":answer}}),
                    json!({"method":"item/completed","params":{"threadId":THREAD,"turnId":turn,"item":final_item}}),
                    json!({"method":"turn/completed","params":{"threadId":THREAD,"turn":{"id":turn,"status":"completed","error":null,"itemsView":"full","items":[item,final_item]}}}),
                    json!({"method":"account/updated","params":{"authMode":"chatgpt"}}),
                ];
                let mut batch = Vec::new();
                for value in values {
                    serde_json::to_writer(&mut batch, &value).map_err(|_| "browser-peer-batch")?;
                    batch.push(b'\n');
                }
                peer.output
                    .write_all(&batch)
                    .map_err(|_| "browser-peer-batch")?;
                peer.output.flush().map_err(|_| "browser-peer-batch")?;
                if let Some(value) = peer.read()? {
                    if value.get("method").and_then(Value::as_str) != Some("thread/unsubscribe")
                        || value["params"] != json!({"threadId":THREAD})
                    {
                        return Err("browser-revocation-unexpected-send");
                    }
                }
                return Ok(());
            }
            peer.browser_complete(turn, &item)?;
        } else {
            let response = peer.read()?.ok_or("browser-peer-response-eof")?;
            if response
                != json!({"id":rpc,"result":{"contentItems":[{"type":"inputText","text":"synthetic readout"}],"success":true}})
            {
                return Err("browser-peer-response-identity");
            }
            let mut done = item;
            done["status"] = json!("completed");
            done["contentItems"] = json!([{"type":"inputText","text":"synthetic readout"}]);
            done["success"] = json!(true);
            done["durationMs"] = json!(1);
            peer.note(
                "item/completed",
                json!({"threadId":THREAD,"turnId":turn,"item":done,"completedAtMs":2}),
            )?;
            peer.browser_complete(turn, &done)?;
        }
    }
    if peer.read()?.is_some() {
        return Err("browser-peer-unexpected-frame");
    }
    Ok(())
}

impl<R: BufRead, W: Write> Peer<R, W> {
    fn browser_complete(&mut self, turn: &str, dynamic: &Value) -> Result<(), &'static str> {
        let answer = "authentic browser final";
        let item = json!({"id":format!("final-{turn}"),"type":"agentMessage","text":answer,"phase":"final_answer"});
        self.note("item/agentMessage/delta",json!({"threadId":THREAD,"turnId":turn,"itemId":format!("final-{turn}"),"delta":answer}))?;
        self.note(
            "item/completed",
            json!({"threadId":THREAD,"turnId":turn,"item":item}),
        )?;
        self.note("turn/completed",json!({"threadId":THREAD,"turn":{"id":turn,"status":"completed","error":null,"itemsView":"full","items":[dynamic,item]}}))?;
        self.close_without_turn()?;
        self.usage_tail(turn)
    }
}
