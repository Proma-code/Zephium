//! Closed-fact diagnostics for the authored journey fixture; no page values or prompts.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use zephium_core::work::model::*;

static IMAGE_REQUESTS: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Client {
    pub(super) inner: Arc<dyn WorkModelClient>,
    pub(super) calls: AtomicUsize,
}
fn quoted(line: &str, key: &str) -> Option<String> {
    let value = line.split_once(key)?.1;
    serde_json::Deserializer::from_str(value)
        .into_iter::<String>()
        .next()?
        .ok()
}
impl WorkModelClient for Client {
    fn call<'a>(
        &'a self,
        request: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> WorkModelFuture<'a> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if request.image_fence.is_some() {
                IMAGE_REQUESTS.fetch_add(1, Ordering::Relaxed);
            }
            let form_offered = request.tools.iter().any(|tool| tool.name == "fill_form");
            let mut controls = Vec::new();
            for message in &request.messages {
                let WorkModelMessage::User(parts) = message else {
                    continue;
                };
                for part in parts {
                    let WorkModelPart::Text(text) = part else {
                        continue;
                    };
                    if !text.starts_with("ZSEM3 ") {
                        continue;
                    }
                    controls.clear();
                    for line in text.lines().take(512) {
                        let Some(reference) = line
                            .strip_prefix("N ")
                            .and_then(|line| line.split_whitespace().next())
                        else {
                            continue;
                        };
                        if !reference.strip_prefix("@a").is_some_and(|digits| {
                            !digits.is_empty()
                                && digits.len() <= 10
                                && digits.bytes().all(|b| b.is_ascii_digit())
                        }) {
                            continue;
                        }
                        let Some(name) = quoted(line, " name=") else {
                            continue;
                        };
                        let (field, expected) = match name.as_str() {
                            "From" => ("from", "Northport"),
                            "To" => ("to", "Lakeside"),
                            "Day" => ("day", "18"),
                            "Month" => ("month", "11"),
                            "Year" => ("year", "2026"),
                            "Hours" => ("hour", "9"),
                            "Minutes" => ("minute", "0"),
                            "Northport" => ("origin_option", ""),
                            "Lakeside" => ("destination_option", ""),
                            "Search" => ("search", ""),
                            _ => continue,
                        };
                        controls.push((
                            reference.to_owned(),
                            field,
                            expected,
                            quoted(line, " value="),
                            line.contains("fill"),
                        ));
                    }
                }
            }
            let click_refs: Vec<String> = request
                .tools
                .iter()
                .filter(|tool| tool.name == "act")
                .filter_map(|tool| {
                    tool.schema
                        .pointer("/properties/actions/items/anyOf")
                        .and_then(serde_json::Value::as_array)
                })
                .flatten()
                .filter(|variant| {
                    variant
                        .pointer("/properties/kind/enum/0")
                        .and_then(serde_json::Value::as_str)
                        == Some("click")
                })
                .filter_map(|variant| {
                    variant
                        .pointer("/properties/target/enum")
                        .and_then(serde_json::Value::as_array)
                })
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect();
            let agrees = |value: Option<&str>, expected: &str| {
                value.is_some_and(|value| {
                    value == expected
                        || (!expected.is_empty()
                            && value
                                .parse::<u16>()
                                .ok()
                                .zip(expected.parse::<u16>().ok())
                                .is_some_and(|(a, b)| a == b))
                })
            };
            let offered: Vec<_> = controls.iter().map(|(reference,field,expected,value,fill)| serde_json::json!({
                "ref":reference,"field":field,"matches":agrees(value.as_deref(),expected),"fill":fill,"click":click_refs.contains(reference)
            })).collect();
            let scripted = std::env::var_os("ZEPHIUM_PROBE_JOURNEY_SCRIPTED").is_some();
            let result = if scripted {
                script(&request, &controls, call)
            } else {
                self.inner.call(request, events).await
            };
            let proposals: Vec<_> = result.as_ref().ok().into_iter().flat_map(|outcome| &outcome.assistant).filter_map(|part| {
                let WorkModelPart::ToolCall(tool) = part else { return None; };
                let name = match tool.name.as_str() { "act" | "fill_form" | "snapshot" | "locate" | "read" | "extract" | "show_for_human" => tool.name.as_str(), _ => "other" };
                let actions: Vec<_> = tool.arguments[if tool.name == "fill_form" { "fields" } else { "actions" }].as_array().into_iter().flatten().take(8).map(|action| {
                    let field = controls.iter().find(|(reference,..)| action["target"].as_str() == Some(reference));
                    serde_json::json!({"field":field.map(|(_,field,..)| *field),
                        "kind":match action["kind"].as_str() { Some("fill" | "click" | "scroll" | "select" | "press" | "text" | "choice" | "date" | "time") => action["kind"].clone(), _ => serde_json::Value::Null },
                        "requested_matches":field.is_some_and(|(_,_,expected,_,_)| agrees(action["value"].as_str(),expected)),
                        "already_equal":field.is_some_and(|(_,_,_,value,_)| value.as_deref().is_some() && value.as_deref() == action["value"].as_str())})
                }).collect();
                Some(serde_json::json!({"tool":name,"actions":actions,"submit_given":tool.arguments.get("submit").is_some_and(|value| !value.is_null()),"submit_ref":tool.arguments.get("submit").and_then(serde_json::Value::as_str).and_then(zephium_agentic::SemanticReferenceId::parse).map(|reference| reference.model_token()),"submit":controls.iter().find(|(reference,..)| tool.arguments["submit"].as_str() == Some(reference)).map(|(_,field,..)| *field)}))
            }).collect();
            use std::io::Write;
            let _ = writeln!(std::io::stdout().lock(), "journey-model: call={call} form_offered={form_offered} scripted={scripted} settled={} controls={} proposals={}; content=synthetic_closed_facts", result.is_ok(), serde_json::json!(offered), serde_json::json!(proposals));
            result
        })
    }
}

fn script(
    request: &WorkModelRequest,
    controls: &[(String, &str, &str, Option<String>, bool)],
    call: usize,
) -> Result<WorkModelOutcome, WorkModelError> {
    use serde_json::json;
    let texts: Vec<_> = request
        .messages
        .iter()
        .filter_map(|message| match message {
            WorkModelMessage::User(parts) => Some(parts),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            WorkModelPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let (name, arguments) = if call == 0 {
        let find = |field| {
            controls
                .iter()
                .find(|(_, found, ..)| *found == field)
                .ok_or(WorkModelError::Protocol)
        };
        let fields = ["from", "to", "day", "month", "year", "hour", "minute"]
            .into_iter()
            .map(|field| {
                find(field).map(|(reference, _, expected, ..)| {
                    json!({"target":reference,
"value":expected})
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        (
            "fill_form",
            json!({"fields":fields,"submit":find("search")?.0}),
        )
    } else if request
        .tools
        .iter()
        .any(|tool| tool.name == "extract_result")
    {
        let input = texts
            .iter()
            .find(|text| text.starts_with("ZEXTRACT"))
            .ok_or(WorkModelError::Protocol)?;
        let field = input
            .lines()
            .find(|line| line.starts_with("S name="))
            .and_then(|line| quoted(line, " name="))
            .ok_or(WorkModelError::Protocol)?;
        let source = input
            .lines()
            .find(|line| line.starts_with("R @r") && line.contains("Train Q17 departs"))
            .ok_or(WorkModelError::Protocol)?;
        let reference = source
            .split_whitespace()
            .nth(1)
            .ok_or(WorkModelError::Protocol)?;
        let offset = source.find('"').ok_or(WorkModelError::Protocol)?;
        let value = serde_json::Deserializer::from_str(&source[offset..])
            .into_iter::<String>()
            .next()
            .and_then(Result::ok)
            .ok_or(WorkModelError::Protocol)?;
        (
            "extract_result",
            json!({"v":1,"schema":1,"fields":[{"name":field,"value":{"k":"text_list","items":[{"value":value,"sources":[reference]}],"sources":[reference]}}]}),
        )
    } else if texts.iter().any(|text| text.contains("Train Q17 departs")) {
        ("extract", json!({"scope":{"kind":"initial"},"schema_id":1}))
    } else {
        return Err(WorkModelError::Protocol);
    };
    Ok(WorkModelOutcome {
        assistant: vec![WorkModelPart::ToolCall(WorkModelToolCall {
            id: format!("journey_fixture_{call}"),
            name: name.into(),
            arguments,
        })],
        stop: WorkModelStop::ToolUse,
        usage: WorkModelUsage {
            input_tokens: 500,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 500,
            reasoning_tokens: 0,
            cost_micros: None,
            ..WorkModelUsage::default()
        },
    })
}

pub(super) async fn run(
    context: &super::Context<'_>,
) -> Result<super::WorkflowResult, &'static str> {
    if std::env::var_os("ZEPHIUM_PROBE_JOURNEY_SCRIPTED").is_some() {
        use std::io::Write;
        let _ = writeln!(
            std::io::stdout().lock(),
            "journey-fixture: scripted=true api_calls=0; reported usage is simulated"
        );
    }
    IMAGE_REQUESTS.store(0, Ordering::Relaxed);
    let before = context.sites.posts();
    let run = context.run("Rail journeys", false, "Allow", vec![vec![super::browse(
        "/journey-form",
        "Find Northport to Lakeside trains on 18 November 2026. Choose the station suggestions, set the departure date and the search time to exactly 09:00, run Search, and report the exact train, departure, duration, changes and fare. Do not book.",
    )]]).await?;
    let found = super::mentions(&run, &["q17", "09:30", "75", "25"]);
    let hour_edits = context
        .sites
        .hits(super::Site::Account, "/journey-hour-edited?")
        .len();
    let hour_matched = context
        .sites
        .hits(super::Site::Account, "/journey-hour-edited?matched=1")
        .len();
    let queried = !context
        .sites
        .hits(super::Site::Account, "/journey-results?")
        .is_empty();
    let held = run.opened.iter().any(|page| page.held_back);
    let posts = context.sites.posts() - before;
    let fields = super::JOURNEY_FIELDS.load(Ordering::SeqCst);
    let selected = ["from", "to"].iter().all(|field| {
        !context
            .sites
            .hits(
                super::Site::Account,
                &format!("/journey-station-chosen?field={field}&matched=1"),
            )
            .is_empty()
    });
    let image_requests = IMAGE_REQUESTS.load(Ordering::Relaxed);
    let passed = image_requests == 0
        && found
        && queried
        && !held
        && posts == 0
        && fields == 127
        && hour_matched > 0
        && selected;
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "loopback-site: check=journey_form found={found} queried={queried} held={held} posts={posts} hour_edits={hour_edits} hour_matched={hour_matched} selected={selected} image_requests={image_requests} field_mask={fields} passed={passed}; content=synthetic");
    let cost: Option<u64> = run
        .state
        .executions
        .iter()
        .flat_map(|execution| &execution.attempts)
        .try_fold(0u64, |sum, attempt| {
            attempt
                .usage
                .as_ref()
                .and_then(|usage| sum.checked_add(u64::from(usage.cost_micro_usd)))
        });
    let calls: u64 = run
        .state
        .executions
        .iter()
        .flat_map(|execution| &execution.steps)
        .filter_map(|step| step.measurements.as_ref())
        .map(|measure| u64::from(measure.planner_calls))
        .sum();
    let _ = writeln!(
        std::io::stdout().lock(),
        "journey-summary: model_calls={calls} cost_micro_usd={cost:?}; basis=attempt_accounting"
    );
    Ok(super::WorkflowResult {
        state: run.state,
        failure: (!passed).then_some("journey_form"),
    })
}
