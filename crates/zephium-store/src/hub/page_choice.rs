//! One durable transaction for the existing Page setting cohort.
use super::*;
use zephium_core::page_choice::{
    keys, Backend, Call, CodexChoice, CodexEffort, CodexEffortChoice, Fault, Reply, Selection,
    Snapshot,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCodexChoice {
    model: String,
    effort: CodexEffort,
}
fn read(conn: &Connection, profile: ProfileId) -> Result<(Snapshot, [Option<String>; 4]), Fault> {
    let mut values: [Option<String>; 4] = std::array::from_fn(|_| None);
    for (key, value) in keys(profile).iter().zip(&mut values) {
        *value = conn
            .query_row("SELECT CASE WHEN length(CAST(value AS BLOB)) <= ?2 THEN value END FROM settings WHERE key=?1", params![key, settings::MAX_SETTING_VALUE_BYTES as i64], |row| row.get::<_, Option<String>>(0))
            .optional().map_err(|_| Fault::Unavailable)?
            .map(|value| value.ok_or(Fault::Corrupt)).transpose()?;
    }
    let model = match values[0].as_deref() {
        None | Some("") => None,
        Some(value) => Some(serde_json::from_str(value).map_err(|_| Fault::Corrupt)?),
    };
    let backend = match values[1].as_deref() {
        None | Some("model") => Backend::Model,
        Some("codex") => Backend::Codex,
        _ => return Err(Fault::Corrupt),
    };
    let codex = match values[2].as_deref() {
        None | Some("") => None,
        Some(value) => Some(match serde_json::from_str::<CodexChoice>(value) {
            Ok(choice) => choice,
            Err(_) => {
                let legacy: LegacyCodexChoice =
                    serde_json::from_str(value).map_err(|_| Fault::Corrupt)?;
                CodexChoice {
                    model: legacy.model,
                    effort: CodexEffortChoice::Explicit(legacy.effort),
                }
            }
        }),
    };
    let revision = match values[3].as_deref() {
        None => 0,
        Some(value) => {
            let number: u64 = value.parse().map_err(|_| Fault::Corrupt)?;
            if number == 0 || number > i64::MAX as u64 || number.to_string() != value {
                return Err(Fault::Corrupt);
            }
            number
        }
    };
    let snapshot = Snapshot {
        profile,
        revision,
        backend,
        model,
        codex,
    };
    snapshot
        .valid()
        .then_some((snapshot, values))
        .ok_or(Fault::Corrupt)
}
impl Hub {
    pub(crate) fn page_choice_call(&mut self, profile: ProfileId, call: Call) -> Reply {
        if !call.valid(profile) {
            return Reply::Failed(Fault::Invalid);
        }
        if !self.registry.contains(&profile) {
            return Reply::Failed(Fault::NotRegistered);
        }
        if self.recovery_required.is_some() || self.degraded_profiles.contains(&profile) {
            return Reply::Failed(Fault::Unavailable);
        }
        let Call::CompareAndSwap {
            expected,
            selection,
        } = call
        else {
            return match read(&self.meta, profile) {
                Ok((snapshot, _)) => Reply::Loaded(snapshot),
                Err(fault) => Reply::Failed(fault),
            };
        };
        let Some(next) = expected.updated(selection.clone()) else {
            return Reply::Failed(Fault::RevisionExhausted);
        };
        let previous: i64 = match self
            .meta
            .pragma_query_value(None, "synchronous", |row| row.get(0))
        {
            Ok(value @ 1..=2) => value,
            _ => return Reply::Failed(Fault::Unavailable),
        };
        if self
            .meta
            .pragma_update(None, "synchronous", "FULL")
            .is_err()
        {
            return Reply::Failed(Fault::Unavailable);
        }
        let result = (|| -> Result<Reply, Fault> {
            let tx = self.meta.transaction().map_err(|_| Fault::OutcomeUnknown)?;
            let (current, mut values) = read(&tx, profile)?;
            if current != *expected {
                return Ok(Reply::Conflict(current));
            }
            // Retain the inactive setting bytes exactly, including legacy format.
            match selection {
                Selection::Model(choice) => {
                    values[0] = Some(match choice {
                        Some(choice) => {
                            serde_json::to_string(&choice).map_err(|_| Fault::Invalid)?
                        }
                        None => String::new(),
                    })
                }
                Selection::Codex(choice) => {
                    values[2] = Some(serde_json::to_string(&choice).map_err(|_| Fault::Invalid)?)
                }
            }
            values[1] = Some(
                match next.backend {
                    Backend::Model => "model",
                    Backend::Codex => "codex",
                }
                .into(),
            );
            values[3] = Some(next.revision.to_string());
            let count: i64 = tx
                .query_row("SELECT count(*) FROM settings", [], |r| r.get(0))
                .map_err(|_| Fault::OutcomeUnknown)?;
            let mut added = 0;
            for (key, value) in keys(profile).iter().zip(&values) {
                if value.is_some() {
                    let exists: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM settings WHERE key=?1)",
                            [key],
                            |r| r.get(0),
                        )
                        .map_err(|_| Fault::OutcomeUnknown)?;
                    added += i64::from(!exists);
                }
            }
            if count + added > settings::MAX_APP_SETTINGS {
                return Ok(Reply::Failed(Fault::Limit));
            }
            for (key, value) in keys(profile).iter().zip(values) {
                if let Some(value) = value {
                    tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map_err(|_| Fault::OutcomeUnknown)?;
                }
            }
            if read(&tx, profile).map_err(|_| Fault::OutcomeUnknown)?.0 != next {
                return Err(Fault::OutcomeUnknown);
            }
            tx.commit().map_err(|_| Fault::OutcomeUnknown)?;
            // Commit is acknowledged before the authoritative readback receipt.
            if read(&self.meta, profile)
                .map_err(|_| Fault::OutcomeUnknown)?
                .0
                != next
            {
                return Err(Fault::OutcomeUnknown);
            }
            Ok(Reply::Committed(next))
        })();
        let restored = self.meta.pragma_update(None, "synchronous", previous);
        if restored.is_err() {
            return Reply::Failed(Fault::OutcomeUnknown);
        }
        result.unwrap_or_else(Reply::Failed)
    }
}

#[cfg(test)]
mod tests;
