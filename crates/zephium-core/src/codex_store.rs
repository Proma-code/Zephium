//! Native-owned Codex continuity records. These values are correlation, never
//! account authority: the caller must retain the validated profile/home lease.
//! No runtime version, executable path, credential, prompt, or auth URL is stored.

pub const MAX_ASSOCIATIONS: usize = 200;
pub const MAX_THREAD_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scope {
    pub incarnation: [u8; 16],
    pub grant_epoch: u64,
}
impl Scope {
    pub fn valid(self) -> bool {
        self.incarnation != [0; 16] && (1..=i64::MAX as u64).contains(&self.grant_epoch)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Terminal {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Dispatch {
    Ready,
    /// The native owner consumed move-only proof that this exact attempt never
    /// entered the original turn write. Not a completion or inferred cancel.
    /// The visible reply remains recorded; no cancelled prompt is replayed.
    Skipped {
        reply: u64,
    },
    /// Surviving this state on reopen is an uncertain prior dispatch, not
    /// permission to replay it. Only an original-runtime receipt may settle it.
    Pending {
        reply: u64,
        turn_id: Option<String>,
    },
    Settled {
        reply: u64,
        turn_id: String,
        outcome: Terminal,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Association {
    pub scope: Scope,
    pub conversation: String,
    /// None only for Skipped before any original thread receipt exists.
    pub thread_id: Option<String>,
    pub revision: u64,
    pub dispatch: Dispatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Call {
    /// Read-only saved-history metadata, including retired grant epochs. This
    /// is not a scope/receipt that permits native resume or dispatch.
    History {
        conversation: String,
    },
    /// First use records the independently validated home incarnation. Existing
    /// different incarnations are never adopted or overwritten.
    Open {
        incarnation: [u8; 16],
    },
    /// Invoke only at the admitted auth-mutating handoff, not a queued intent.
    /// A committed advance retires old mappings without deleting their history.
    AdvanceGrant {
        expected: Scope,
    },
    Load {
        scope: Scope,
        conversation: String,
    },
    /// Persist the original thread receipt before admitting its first turn.
    /// None revision requires absence; Some fills only exact SkippedNone while
    /// preserving its visible unsent reply. Existing threads never remap.
    Bind {
        scope: Scope,
        conversation: String,
        thread_id: String,
        expected_revision: Option<u64>,
    },
    /// Only the native same-attempt NeverStarted proof permits this barrier.
    /// Missing revision requires absence. Existing Pending requires the exact
    /// same reply and no original turn ID; generic transport/cancel is not proof.
    Skip {
        scope: Scope,
        conversation: String,
        expected_revision: Option<u64>,
        reply: u64,
    },
    /// Persist before handing a new turn to the original process. The revision
    /// and increasing Page reply prevent a duplicate/replayed write.
    Begin {
        scope: Scope,
        conversation: String,
        expected_revision: u64,
        reply: u64,
    },
    RecordTurn {
        scope: Scope,
        conversation: String,
        expected_revision: u64,
        reply: u64,
        turn_id: String,
    },
    /// The original terminal receipt, not optimistic UI text, permits this CAS.
    Settle {
        scope: Scope,
        conversation: String,
        expected_revision: u64,
        reply: u64,
        turn_id: String,
        outcome: Terminal,
    },
}
impl Call {
    pub fn valid(&self) -> bool {
        let number = |n: u64| (1..=i64::MAX as u64).contains(&n);
        match self {
            Self::History { conversation } => valid_conversation(conversation),
            Self::Open { incarnation } => *incarnation != [0; 16],
            Self::AdvanceGrant { expected } => expected.valid(),
            Self::Load {
                scope,
                conversation,
            } => scope.valid() && valid_conversation(conversation),
            Self::Bind {
                scope,
                conversation,
                thread_id,
                expected_revision,
            } => {
                scope.valid()
                    && valid_conversation(conversation)
                    && valid_provider_id(thread_id)
                    && expected_revision.is_none_or(number)
            }
            Self::Skip {
                scope,
                conversation,
                expected_revision,
                reply,
            } => {
                scope.valid()
                    && valid_conversation(conversation)
                    && expected_revision.is_none_or(number)
                    && number(*reply)
            }
            Self::Begin {
                scope,
                conversation,
                expected_revision,
                reply,
            } => {
                scope.valid()
                    && valid_conversation(conversation)
                    && number(*expected_revision)
                    && number(*reply)
            }
            Self::RecordTurn {
                scope,
                conversation,
                expected_revision,
                reply,
                turn_id,
            }
            | Self::Settle {
                scope,
                conversation,
                expected_revision,
                reply,
                turn_id,
                ..
            } => {
                scope.valid()
                    && valid_conversation(conversation)
                    && number(*expected_revision)
                    && number(*reply)
                    && valid_provider_id(turn_id)
            }
        }
    }
    pub fn mutates(&self) -> bool {
        !matches!(self, Self::Load { .. } | Self::History { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    Invalid,
    NotRegistered,
    Unavailable,
    IncarnationMismatch,
    StaleGrant,
    Conflict,
    Limit,
    Corrupt,
    /// The actor accepted the call but its durable answer was not observed.
    /// Reconcile; do not send, recreate, advance again, or replay on this result.
    OutcomeUnknown,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reply {
    Scope(Scope),
    Association(Option<Association>),
    Failed(Fault),
}

pub fn valid_conversation(value: &str) -> bool {
    (1..=crate::ports::store::MAX_CHAT_ID_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
pub fn valid_provider_id(value: &str) -> bool {
    (1..=MAX_THREAD_ID_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_identity_and_counters_are_bounded_without_runtime_version() {
        let scope = Scope {
            incarnation: [1; 16],
            grant_epoch: 1,
        };
        assert!(
            Call::Bind {
                scope,
                conversation: "page-1".into(),
                thread_id: "thread-1".into(),
                expected_revision: None
            }
            .valid()
        );
        assert!(
            !Call::Open {
                incarnation: [0; 16]
            }
            .valid()
        );
        assert!(
            !Call::AdvanceGrant {
                expected: Scope {
                    grant_epoch: 0,
                    ..scope
                }
            }
            .valid()
        );
        assert!(
            !Call::Begin {
                scope,
                conversation: "page-1".into(),
                expected_revision: 1,
                reply: 0
            }
            .valid()
        );
        for id in ["", "../thread", "thread\n", "thread other"] {
            assert!(!valid_provider_id(id));
        }
        assert!(valid_provider_id("01234567-abcd_1234"));
        for revision in [None, Some(1)] {
            assert!(
                Call::Skip {
                    scope,
                    conversation: "page-1".into(),
                    expected_revision: revision,
                    reply: 2
                }
                .valid()
            );
        }
        for (revision, reply) in [
            (Some(0), 2),
            (None, 0),
            (Some(u64::MAX), 2),
            (None, u64::MAX),
        ] {
            assert!(
                !Call::Skip {
                    scope,
                    conversation: "page-1".into(),
                    expected_revision: revision,
                    reply
                }
                .valid()
            );
        }
        assert!(
            !Call::Bind {
                scope,
                conversation: "page-1".into(),
                thread_id: "original".into(),
                expected_revision: Some(0)
            }
            .valid()
        );
    }
}
