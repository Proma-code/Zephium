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
            _ => None,
        }
    }
}
struct Peer<R, W> {
    input: R,
    output: W,
    last: u64,
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
        self.reply(id,json!({"data":[{"id":"synthetic-picker","model":MODEL,"displayName":"Synthetic","description":"owned fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"fixture"}],"defaultReasoningEffort":"high","isDefault":true,"inputModalities":["text"]}],"nextCursor":null}))
    }
    fn opened(&mut self, resume: bool, text: &str, defer: bool) -> Result<(), &'static str> {
        let method = if resume {
            "thread/resume"
        } else {
            "thread/start"
        };
        let (id, actual) = self.request(method)?;
        let text = crate::text_policy::prepare_text(text.into()).map_err(|_| "transcript-text")?;
        let mut config = text.fixed_config();
        config["model_reasoning_effort"] = json!("high");
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
        if defer {
            return self.send(json!({"id":id,"error":{"code":-32600,"message":format!("thread {THREAD} is closing; retry thread/resume after the thread is closed")}}));
        }
        self.reply(id,json!({"thread":{"id":THREAD,"ephemeral":false,"status":{"type":"idle"},"environments":[]},"model":MODEL,"modelProvider":"openai","reasoningEffort":"high","cwd":self.cwd,"approvalPolicy":"never","sandbox":{"type":"readOnly","networkAccess":false}}))
    }
    fn start(&mut self, text: &str, turn: &str) -> Result<(), &'static str> {
        let id=self.expect(json!({"method":"turn/start","params":{"threadId":THREAD,"input":[{"type":"text","text":text,"text_elements":[]}],"model":MODEL,"effort":"high","environments":[]}}))?;
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
    fn complete(&mut self, turn: &str, answer: &str, status: &str) -> Result<(), &'static str> {
        self.terminal(turn, answer, status)?;
        let id =
            self.expect(json!({"method":"thread/unsubscribe","params":{"threadId":THREAD}}))?;
        // Real matching close and ACK are independently necessary; test opposite order.
        self.note("thread/closed", json!({"threadId":THREAD}))?;
        self.reply(id, json!({"status":"unsubscribed"}))
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
    peer.opened(false, "synthetic first $skill_one", false)?;
    if matches!(
        scenario,
        Scenario::CancelBeforeTurn | Scenario::RevisionBeforeWrite
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
        | Scenario::RevisionBeforeWrite => {
            unreachable!()
        }
    }
    if peer.read()?.is_some() {
        return Err("transcript-unexpected-replay");
    }
    Ok(())
}
