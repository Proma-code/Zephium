//! One explicit Work run's protected model-source admission. The scope is a
//! profile/connection, not a role, model name or individual resolved client.
//! It never touches persistent credentials or another Work run's state.
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use zephium_core::ids::ProfileId;
use zephium_core::work::model::*;

use super::WorkLeadModels;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Connection {
    // Each profile has exactly one native ChatGPT connection slot. A native
    // account replacement revokes its old leases; it cannot revive this run.
    ChatGpt,
    Gateway(String),
    InvalidGateway,
}
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Source {
    profile: ProfileId,
    connection: Connection,
}
#[derive(Clone, Copy)]
enum Kind {
    ChatGpt,
    Gateway,
}
impl Kind {
    fn uncertain(self) -> WorkModelError {
        match self {
            // Protocol is the existing non-retriable unknown-outcome signal.
            // Do not downgrade an unobserved dispatched request to a refusal.
            Self::ChatGpt => WorkModelError::Protocol,
            Self::Gateway => WorkModelError::Gateway {
                fault: GatewayModelFault::Unavailable,
                usage_unknown: true,
            },
        }
    }
    fn unsupported(self) -> WorkModelError {
        match self {
            Self::ChatGpt => WorkModelError::ChatGpt {
                fault: ChatGptModelFault::Unsupported,
            },
            Self::Gateway => WorkModelError::Gateway {
                fault: GatewayModelFault::Unsupported,
                usage_unknown: false,
            },
        }
    }
    fn terminal(self, error: WorkModelError) -> WorkModelError {
        match (self, error) {
            (Self::ChatGpt, error @ WorkModelError::ChatGpt { .. })
            | (Self::Gateway, error @ WorkModelError::Gateway { .. }) => error,
            // A protected client must not leak a retriable API error through
            // this boundary, including a cancellation with unknown dispatch.
            _ => self.uncertain(),
        }
    }
}

fn source(profile: ProfileId, entry: &WorkModelEntry) -> Option<(Source, Kind, bool)> {
    if let Some(model) = entry.id.strip_prefix("chatgpt/") {
        let valid = !model.is_empty()
            && model == entry.model.model
            && entry.model.provider == WorkModelProvider::OpenAi
            && entry.model.wire == WorkModelWire::OpenAiResponses;
        return Some((
            Source {
                profile,
                connection: Connection::ChatGpt,
            },
            Kind::ChatGpt,
            valid,
        ));
    }
    let id = entry.id.strip_prefix("gateway/")?;
    let (connection, valid) = match id.split_once('/') {
        Some((scope, model))
            if scope.len() == 32
                && scope
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            (
                Connection::Gateway(scope.into()),
                !model.is_empty()
                    && model == entry.model.model
                    && entry.model.provider == WorkModelProvider::Compatible
                    && entry.model.wire == WorkModelWire::ChatCompletions,
            )
        }
        _ => (Connection::InvalidGateway, false),
    };
    Some((
        Source {
            profile,
            connection,
        },
        Kind::Gateway,
        valid,
    ))
}

type SharedGate = Arc<Mutex<Option<WorkModelError>>>;
struct GuardedClient {
    inner: Arc<dyn WorkModelClient>,
    gate: SharedGate,
    kind: Kind,
    valid: bool,
}
impl WorkModelClient for GuardedClient {
    fn call<'a>(
        &'a self,
        request: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> WorkModelFuture<'a> {
        Box::pin(async move {
            // Waiting does not mutate the gate. Dropping a queued waiter must
            // not poison the active owner's successful response.
            let mut terminal = self.gate.lock().await;
            if let Some(error) = *terminal {
                return Err(error);
            }
            if !self.valid {
                let error = self.kind.unsupported();
                *terminal = Some(error);
                return Err(error);
            }
            // Hold the source lock across this call. Only one role/model may
            // dispatch on it at a time, so no sibling can emit tools after an
            // earlier sibling's failure. A dropped dispatched owner leaves the
            // marker intact; dropping its future also drops the inner future.
            // Upstream charges already dispatched cannot be rolled back here.
            *terminal = Some(self.kind.uncertain());
            match self.inner.call(request, events).await {
                Ok(outcome) => {
                    // This is still the same exclusive owner: no other failure
                    // can be overwritten by its successful completion.
                    *terminal = None;
                    Ok(outcome)
                }
                Err(error) => {
                    let error = self.kind.terminal(error);
                    *terminal = Some(error);
                    Err(error)
                }
            }
        })
    }
}

/// Wrap all roles once for a newly admitted explicit Work run, before cloning
/// clients into page/decision/search adapters. Same-source model variants share
/// a terminal latch; different connections and ordinary API clients do not.
/// Fresh explicit Work must resolve fresh native clients and call this again.
pub fn guard_run_sources(profile: ProfileId, mut models: WorkLeadModels) -> WorkLeadModels {
    let mut gates: HashMap<Source, SharedGate> = HashMap::new();
    for model in [&mut models.lead, &mut models.page, &mut models.light] {
        let Some((source, kind, valid)) = source(profile, &model.entry) else {
            continue;
        };
        let gate = gates
            .entry(source)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();
        model.client = Arc::new(GuardedClient {
            inner: model.client.clone(),
            gate,
            kind,
            valid,
        });
    }
    models
}

#[cfg(test)]
mod tests;
