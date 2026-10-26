//! Serialized, profile-bound barriers for original Codex thread continuity.
use super::*;
use zephium_core::codex_store::{
    Association, Call, Dispatch, Fault, Reply, Scope, Terminal, MAX_ASSOCIATIONS,
};

impl Hub {
    pub(crate) fn codex_call(&mut self, profile: ProfileId, call: Call) -> Reply {
        if !call.valid() {
            return Reply::Failed(Fault::Invalid);
        }
        if !self.registry.contains(&profile) {
            return Reply::Failed(Fault::NotRegistered);
        }
        if self.degraded_profiles.contains(&profile) || self.recovery_required.is_some() {
            return Reply::Failed(Fault::Unavailable);
        }
        let conn = match self.profile_conn(profile) {
            Ok(conn) => conn,
            Err(_) => return Reply::Failed(Fault::Unavailable),
        };
        if !call.mutates() {
            return execute(conn, call).unwrap_or(Reply::Failed(Fault::Corrupt));
        }
        // Ordinary history uses NORMAL. These external-dispatch barriers need
        // FULL before BEGIN; changing synchronous inside a transaction is invalid.
        let previous: i64 = match conn.pragma_query_value(None, "synchronous", |row| row.get(0)) {
            Ok(value @ 1..=2) => value,
            _ => return Reply::Failed(Fault::Unavailable),
        };
        if conn.pragma_update(None, "synchronous", "FULL").is_err() {
            return Reply::Failed(Fault::Unavailable);
        }
        let result = (|| {
            let tx = conn.transaction()?;
            let reply = execute(&tx, call)?;
            tx.commit()?;
            Ok::<_, rusqlite::Error>(reply)
        })();
        let restored = conn.pragma_update(None, "synchronous", previous);
        match (result, restored) {
            (Ok(reply), Ok(())) => reply,
            // A failed commit/acknowledgment may follow changed durable bytes.
            // Never let the caller infer that it is safe to dispatch or retry.
            _ => Reply::Failed(Fault::OutcomeUnknown),
        }
    }
}
fn failed(fault: Fault) -> rusqlite::Result<Reply> {
    Ok(Reply::Failed(fault))
}
fn scope(conn: &Connection) -> rusqlite::Result<Option<Scope>> {
    conn.query_row(
        "SELECT schema_version, incarnation, grant_epoch FROM codex_owner WHERE singleton = 1",
        [],
        |row| {
            let version: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            let epoch: i64 = row.get(2)?;
            let incarnation: [u8; 16] = bytes
                .try_into()
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let value = Scope {
                incarnation,
                grant_epoch: u64::try_from(epoch).map_err(|_| rusqlite::Error::InvalidQuery)?,
            };
            if version != 1 || !value.valid() {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Ok(value)
        },
    )
    .optional()
}
fn read(conn: &Connection, conversation: &str) -> rusqlite::Result<Option<Association>> {
    conn.query_row("SELECT incarnation, grant_epoch, thread_id, revision, dispatch, reply, turn_id, outcome FROM codex_conversations WHERE conversation = ?1", [conversation], |row| {
        let incarnation: Vec<u8> = row.get(0)?;
        let epoch: i64 = row.get(1)?;
        let thread_id: String = row.get(2)?;
        let revision: i64 = row.get(3)?;
        let state: i64 = row.get(4)?;
        let reply: i64 = row.get(5)?;
        let turn: Option<String> = row.get(6)?;
        let outcome: Option<i64> = row.get(7)?;
        let scope = Scope {incarnation: incarnation.try_into().map_err(|_|rusqlite::Error::InvalidQuery)?,grant_epoch:u64::try_from(epoch).map_err(|_|rusqlite::Error::InvalidQuery)?};
        if !scope.valid() || revision < 1 || !zephium_core::codex_store::valid_provider_id(&thread_id)
            || turn.as_ref().is_some_and(|id| !zephium_core::codex_store::valid_provider_id(id)) { return Err(rusqlite::Error::InvalidQuery); }
        let dispatch = match (state, reply, turn, outcome) {
            (0,0,None,None) => Dispatch::Ready,
            (1,reply,turn_id,None) if reply > 0 => Dispatch::Pending {reply:reply as u64,turn_id},
            (2,reply,Some(turn_id),Some(outcome)) if reply > 0 => Dispatch::Settled {reply:reply as u64,turn_id,outcome:match outcome {1=>Terminal::Completed,2=>Terminal::Interrupted,3=>Terminal::Failed,_=>return Err(rusqlite::Error::InvalidQuery)}},
            _=>return Err(rusqlite::Error::InvalidQuery),
        };
        Ok(Association {scope,conversation:conversation.into(),thread_id,revision:revision as u64,dispatch})
    }).optional()
}
fn put(conn: &Connection, value: &Association) -> rusqlite::Result<()> {
    let (state, reply, turn, outcome) = match &value.dispatch {
        Dispatch::Ready => (0, 0, None, None),
        Dispatch::Pending { reply, turn_id } => (1, *reply as i64, turn_id.as_deref(), None),
        Dispatch::Settled {
            reply,
            turn_id,
            outcome,
        } => (
            2,
            *reply as i64,
            Some(turn_id.as_str()),
            Some(match outcome {
                Terminal::Completed => 1,
                Terminal::Interrupted => 2,
                Terminal::Failed => 3,
            }),
        ),
    };
    conn.execute("INSERT INTO codex_conversations(conversation,incarnation,grant_epoch,thread_id,revision,dispatch,reply,turn_id,outcome) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(conversation) DO UPDATE SET revision=excluded.revision,dispatch=excluded.dispatch,reply=excluded.reply,turn_id=excluded.turn_id,outcome=excluded.outcome", params![value.conversation,value.scope.incarnation.as_slice(),value.scope.grant_epoch as i64,value.thread_id,value.revision as i64,state,reply,turn,outcome])?;
    Ok(())
}
fn execute(conn: &Connection, call: Call) -> rusqlite::Result<Reply> {
    if let Call::History { conversation } = &call {
        let value = read(conn, conversation)?;
        if let Some(value) = &value {
            let Some(current) = scope(conn)? else {
                return failed(Fault::Corrupt);
            };
            if value.scope.incarnation != current.incarnation
                || value.scope.grant_epoch > current.grant_epoch
            {
                return failed(Fault::Corrupt);
            }
        }
        return Ok(Reply::Association(value));
    }
    if let Call::Open { incarnation } = call {
        return match scope(conn)? {
            Some(current) if current.incarnation == incarnation => Ok(Reply::Scope(current)),
            Some(_) => failed(Fault::IncarnationMismatch),
            None => {
                // No partial/orphan mapping may be silently adopted.
                let count: i64 =
                    conn.query_row("SELECT count(*) FROM codex_conversations", [], |r| r.get(0))?;
                if count != 0 {
                    return failed(Fault::Corrupt);
                }
                conn.execute("INSERT INTO codex_owner(singleton,schema_version,incarnation,grant_epoch) VALUES(1,1,?1,1)",[incarnation.as_slice()])?;
                Ok(Reply::Scope(Scope {
                    incarnation,
                    grant_epoch: 1,
                }))
            }
        };
    }
    let expected = match &call {
        Call::AdvanceGrant { expected } => *expected,
        Call::Load { scope, .. }
        | Call::Bind { scope, .. }
        | Call::Begin { scope, .. }
        | Call::RecordTurn { scope, .. }
        | Call::Settle { scope, .. } => *scope,
        Call::Open { .. } | Call::History { .. } => unreachable!(),
    };
    let Some(current) = scope(conn)? else {
        return failed(Fault::Conflict);
    };
    if current.incarnation != expected.incarnation {
        return failed(Fault::IncarnationMismatch);
    }
    if current != expected {
        return failed(Fault::StaleGrant);
    }
    if let Call::AdvanceGrant { .. } = call {
        let Some(next) = current
            .grant_epoch
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
        else {
            return failed(Fault::Limit);
        };
        conn.execute(
            "UPDATE codex_owner SET grant_epoch=?1 WHERE singleton=1",
            [next as i64],
        )?;
        return Ok(Reply::Scope(Scope {
            grant_epoch: next,
            ..current
        }));
    }
    let conversation = match &call {
        Call::Load { conversation, .. }
        | Call::Bind { conversation, .. }
        | Call::Begin { conversation, .. }
        | Call::RecordTurn { conversation, .. }
        | Call::Settle { conversation, .. } => conversation,
        _ => unreachable!(),
    };
    let existing = read(conn, conversation)?;
    if existing.as_ref().is_some_and(|a| a.scope != current) {
        return failed(Fault::StaleGrant);
    }
    if matches!(call, Call::Load { .. }) {
        return Ok(Reply::Association(existing));
    }
    if let Call::Bind {
        conversation,
        thread_id,
        ..
    } = call
    {
        if existing.is_some() {
            return failed(Fault::Conflict);
        }
        let count: i64 =
            conn.query_row("SELECT count(*) FROM codex_conversations", [], |r| r.get(0))?;
        if count >= MAX_ASSOCIATIONS as i64 {
            return failed(Fault::Limit);
        }
        let duplicate: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM codex_conversations WHERE thread_id=?1)",
            [&thread_id],
            |r| r.get(0),
        )?;
        if duplicate {
            return failed(Fault::Conflict);
        }
        let value = Association {
            scope: current,
            conversation,
            thread_id,
            revision: 1,
            dispatch: Dispatch::Ready,
        };
        put(conn, &value)?;
        return Ok(Reply::Association(Some(value)));
    }
    let Some(mut value) = existing else {
        return failed(Fault::Conflict);
    };
    let revision = match &call {
        Call::Begin {
            expected_revision, ..
        }
        | Call::RecordTurn {
            expected_revision, ..
        }
        | Call::Settle {
            expected_revision, ..
        } => *expected_revision,
        _ => unreachable!(),
    };
    if value.revision != revision {
        return failed(Fault::Conflict);
    }
    let Some(next_revision) = revision.checked_add(1).filter(|v| *v <= i64::MAX as u64) else {
        return failed(Fault::Limit);
    };
    value.dispatch = match call {
        Call::Begin { reply, .. } => match value.dispatch {
            Dispatch::Ready => Dispatch::Pending {
                reply,
                turn_id: None,
            },
            Dispatch::Settled {
                reply: previous, ..
            } if reply > previous => Dispatch::Pending {
                reply,
                turn_id: None,
            },
            _ => return failed(Fault::Conflict),
        },
        Call::RecordTurn { reply, turn_id, .. } => match value.dispatch {
            Dispatch::Pending {
                reply: expected,
                turn_id: None,
            } if reply == expected => Dispatch::Pending {
                reply,
                turn_id: Some(turn_id),
            },
            _ => return failed(Fault::Conflict),
        },
        Call::Settle {
            reply,
            turn_id,
            outcome,
            ..
        } => match value.dispatch {
            Dispatch::Pending {
                reply: expected,
                turn_id: recorded,
            } if reply == expected && recorded.as_ref().is_none_or(|id| id == &turn_id) => {
                Dispatch::Settled {
                    reply,
                    turn_id,
                    outcome,
                }
            }
            _ => return failed(Fault::Conflict),
        },
        _ => unreachable!(),
    };
    value.revision = next_revision;
    put(conn, &value)?;
    Ok(Reply::Association(Some(value)))
}

#[cfg(test)]
mod tests;
