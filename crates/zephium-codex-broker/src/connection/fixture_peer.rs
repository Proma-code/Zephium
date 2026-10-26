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
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "owner-two-turns" => Some(Self::TwoTurns),
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
            json!({"account":{"type":"chatgpt"},"requiresOpenaiAuth":true}),
        )?;
        let id = self.expect(crate::wire::model_list(0, None))?;
        self.reply(id,json!({"data":[{"id":"synthetic-picker","model":MODEL,"displayName":"Synthetic","description":"owned fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"fixture"},{"reasoningEffort":"none","description":"explicit none"}],"defaultReasoningEffort":"high","isDefault":true,"inputModalities":["text"]}],"nextCursor":null}))
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
        default_choices: matches!(scenario, Scenario::TwoTurns),
        home,
        cwd,
    };
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
        Scenario::TwoTurns => {
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
        Scenario::LostStartAck
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
