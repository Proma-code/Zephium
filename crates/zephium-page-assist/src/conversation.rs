//! One conversation: the person's messages and the assistant's replies, and every
//! change to them as numbered events. The snapshot the panel reads and the
//! form that is stored are the same value.
use crate::ipc::{
    PageAssistAttachmentV1 as Attachment, PageAssistBlockV1 as Block,
    PageAssistChangedV1 as Changed, PageAssistConversationV1 as View, PageAssistEventV1 as Event,
    PageAssistMessageV1 as Message, PageAssistReplyStateV1 as ReplyState,
    PageAssistSuggestionV1 as Suggestion,
};
use crate::text;

/// Most messages kept; older ones leave in pairs.
pub const MAX_MESSAGES: usize = 160;
/// Most blocks one reply holds.
pub const MAX_BLOCKS: usize = 24;
/// Longest text block, in bytes.
pub const MAX_TEXT_BYTES: usize = 24 * 1024;
/// Longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 60;

/// Original-client outcome, independent from Page action admission or cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeConversationStatus {
    /// The original runtime confirmed completion.
    Completed,
    /// Exact original-writer no-send proof and durable Skip acknowledgement.
    /// Not derived from cancellation, timeout, or an absent provider turn ID.
    NeverStarted,
    /// Genuine original terminal; local durable history settlement is unconfirmed.
    HistoryUncertain,
    /// The original runtime confirmed interruption.
    Interrupted,
    /// The original runtime confirmed failure.
    Failed,
    /// The original outcome is not established; never a completed reply.
    Unknown,
}

/// Bounded original-provider final text plus a separate local Page failure.
/// This is native output data, not authority to admit effects or to restore a run.
pub struct NativeTerminalProjection {
    /// Existing native text block; cards and action blocks are not replaceable.
    pub block: u16,
    /// The original result, never inferred from local cancellation.
    pub status: NativeConversationStatus,
    /// Exact completed-item selection after citation rewriting. Absent text clears
    /// a known completed answer, but preserves partial text for an unknown/failed turn.
    pub final_text: Option<String>,
    /// Citations proven by the same reply's retained read ledger.
    pub cites: Vec<crate::ipc::PageAssistCiteV1>,
    /// Local delivery/action failure, independent from the provider result.
    pub local_fault: Option<crate::ipc::PageAssistNativeFaultV1>,
}

/// A conversation and its event counter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conversation {
    space: String,
    view: View,
    next: u32,
}

impl Conversation {
    /// An empty conversation in `space`.
    pub fn new(id: String, space: String) -> Self {
        Self {
            space,
            view: View {
                id,
                title: None,
                messages: Vec::new(),
                seq: 0,
            },
            next: 1,
        }
    }

    /// A stored conversation, opened again: nothing in it is still running,
    /// and no earlier reply jumps to a page any more.
    pub fn restore(mut view: View, space: String) -> Self {
        let mut next = 1;
        for message in &mut view.messages {
            match message {
                Message::Person { id, .. } => next = next.max(*id + 1),
                Message::Reply {
                    id,
                    state,
                    phase,
                    inert,
                    ..
                } => {
                    next = next.max(*id + 1);
                    *phase = None;
                    *inert = true;
                    if matches!(
                        state,
                        ReplyState::Working | ReplyState::Waiting | ReplyState::Paused
                    ) {
                        *state = ReplyState::Cancelled;
                    }
                }
            }
        }
        Self { space, view, next }
    }

    /// Its id.
    pub fn id(&self) -> &str {
        &self.view.id
    }

    /// The Space it belongs to.
    pub fn space(&self) -> &str {
        &self.space
    }

    /// The snapshot.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// Whether nothing was said yet.
    pub fn is_empty(&self) -> bool {
        self.view.messages.is_empty()
    }

    /// The messages, oldest first.
    pub fn messages(&self) -> &[Message] {
        &self.view.messages
    }

    /// One reply's blocks.
    pub fn blocks(&self, reply: u32) -> Option<&[Block]> {
        self.view.messages.iter().find_map(|message| match message {
            Message::Reply { id, blocks, .. } if *id == reply => Some(blocks.as_slice()),
            _ => None,
        })
    }

    /// The latest reply's id.
    pub fn latest_reply(&self) -> Option<u32> {
        self.view
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Reply { id, .. } => Some(*id),
                Message::Person { .. } => None,
            })
    }

    fn changed(&mut self, reply: u32, kind: Event) -> Changed {
        self.view.seq = self.view.seq.wrapping_add(1);
        Changed {
            space: self.space.clone(),
            conversation: self.view.id.clone(),
            reply,
            seq: self.view.seq,
            kind,
        }
    }

    fn trim(&mut self) {
        while self.view.messages.len() > MAX_MESSAGES {
            self.view.messages.remove(0);
            if matches!(self.view.messages.first(), Some(Message::Reply { .. })) {
                self.view.messages.remove(0);
            }
        }
    }

    /// The person's message joins; the first one names the conversation.
    pub fn say(
        &mut self,
        words: &str,
        suggestion: Option<Suggestion>,
        attachments: Vec<Attachment>,
        title: Option<&str>,
    ) -> Vec<Changed> {
        let id = self.next;
        self.next += 1;
        let message = Message::Person {
            id,
            text: words.to_owned(),
            suggestion,
            attachments,
        };
        self.view.messages.push(message.clone());
        self.trim();
        let mut events = vec![self.changed(
            id,
            Event::Message {
                message: Box::new(message),
            },
        )];
        if self.view.title.is_none() {
            let title = text::bounded(&text::clean(title.unwrap_or(words)), MAX_TITLE_CHARS);
            if !title.is_empty() {
                self.view.title = Some(title.clone());
                events.push(self.changed(0, Event::Titled { title }));
            }
        }
        events
    }

    /// A reply begins, working.
    pub fn begin_reply(&mut self) -> (u32, Changed) {
        let id = self.next;
        self.next += 1;
        let message = Message::Reply {
            id,
            state: ReplyState::Working,
            phase: None,
            blocks: Vec::new(),
            cost: None,
            fault: None,
            inert: false,
        };
        self.view.messages.push(message.clone());
        self.trim();
        let event = self.changed(
            id,
            Event::Message {
                message: Box::new(message),
            },
        );
        (id, event)
    }

    /// Folds `kind` into reply `reply`; none when it does not apply.
    pub fn apply(&mut self, reply: u32, kind: Event) -> Option<Changed> {
        if let Event::Inert = kind {
            let mut changed = false;
            for message in &mut self.view.messages {
                if let Message::Reply { id, inert, .. } = message {
                    if *id <= reply && !*inert {
                        *inert = true;
                        changed = true;
                    }
                }
            }
            return changed.then(|| self.changed(reply, kind));
        }
        let message = self
            .view
            .messages
            .iter_mut()
            .find(|message| matches!(message, Message::Reply { id, .. } if *id == reply))?;
        let Message::Reply {
            state,
            phase,
            blocks,
            cost,
            fault,
            ..
        } = message
        else {
            return None;
        };
        match &kind {
            Event::Phase { phase: next } => *phase = next.clone(),
            Event::Text { block, delta } => {
                let index = usize::from(*block);
                if index == blocks.len() && index < MAX_BLOCKS {
                    blocks.push(Block::Text {
                        text: String::new(),
                        cites: Vec::new(),
                    });
                }
                let Some(Block::Text { text, .. }) = blocks.get_mut(index) else {
                    return None;
                };
                if text.len() + delta.len() > MAX_TEXT_BYTES {
                    return None;
                }
                text.push_str(delta);
            }
            Event::TextSnapshot { block, text: next } => {
                if !matches!(
                    state,
                    ReplyState::Working | ReplyState::Waiting | ReplyState::Paused
                ) || next.len() > MAX_TEXT_BYTES
                {
                    return None;
                }
                let index = usize::from(*block);
                if index == blocks.len() && index < MAX_BLOCKS {
                    blocks.push(Block::Text {
                        text: String::new(),
                        cites: Vec::new(),
                    });
                }
                let Some(Block::Text { text, cites }) = blocks.get_mut(index) else {
                    return None;
                };
                *text = next.clone();
                cites.clear();
            }
            Event::Cites { block, cites: next } => match blocks.get_mut(usize::from(*block)) {
                Some(Block::Text { cites, .. }) => *cites = next.clone(),
                _ => return None,
            },
            Event::Block { block, value } => {
                let index = usize::from(*block);
                if matches!(value, Block::Text { .. }) {
                    return None;
                }
                match index.cmp(&blocks.len()) {
                    std::cmp::Ordering::Less => blocks[index] = value.clone(),
                    std::cmp::Ordering::Equal if index < MAX_BLOCKS => blocks.push(value.clone()),
                    _ => return None,
                }
            }
            Event::State { state: next } => *state = *next,
            Event::Done { cost: next } => {
                *state = ReplyState::Done;
                *phase = None;
                *cost = next.clone();
            }
            Event::Cost { cost: next } => *cost = Some(next.clone()),
            Event::Failed { fault: next } => {
                *state = ReplyState::Failed;
                *phase = None;
                *fault = Some(*next);
            }
            Event::Cancelled => {
                *state = ReplyState::Cancelled;
                *phase = None;
            }
            Event::Message { .. } | Event::Titled { .. } | Event::Invalidated | Event::Inert => {
                return None;
            }
        }
        Some(self.changed(reply, kind))
    }

    /// Projects one sealed original-runtime result without reopening Page effects.
    ///
    /// The caller must hold the exact live native reply/foreground/profile identity
    /// and consume its settlement only once. Unlike streaming `TextSnapshot`, this
    /// may replace final text after a local cancellation/failure. It emits a reload
    /// notification rather than replaying an artificial Working state. The local
    /// fault is separate from the original status; unknown provider outcomes remain
    /// unknown. Citations must come from that reply's retained citation book.
    pub fn project_native_terminal(
        &mut self,
        reply: u32,
        projection: NativeTerminalProjection,
    ) -> Option<Changed> {
        let NativeTerminalProjection {
            block,
            status,
            final_text,
            cites,
            local_fault,
        } = projection;
        use crate::ipc::{PageAssistFaultV1 as Fault, PageAssistNativeFaultV1 as NativeFault};
        if (status == NativeConversationStatus::Completed
            && local_fault.is_some_and(|fault| {
                !matches!(
                    fault,
                    NativeFault::LocalInterruption | NativeFault::Limit | NativeFault::Protocol
                )
            }))
            || (status == NativeConversationStatus::NeverStarted && final_text.is_some())
            || (final_text.is_none() && !cites.is_empty())
            || final_text
                .as_ref()
                .is_some_and(|text| text.len() > MAX_TEXT_BYTES)
            || cites.len() > crate::citations::MAX_SOURCES
            || cites.iter().enumerate().any(|(index, cite)| {
                cite.n == 0
                    || usize::from(cite.n) > crate::citations::MAX_SOURCES
                    || cites[..index].iter().any(|previous| previous.n == cite.n)
                    || cite.excerpt.chars().count() > crate::ipc::PAGE_ASSIST_MAX_EXCERPT_CHARS
            })
        {
            return None;
        }
        let message = self
            .view
            .messages
            .iter_mut()
            .find(|message| matches!(message, Message::Reply { id, .. } if *id == reply))?;
        let Message::Reply {
            state,
            phase,
            blocks,
            cost,
            fault,
            ..
        } = message
        else {
            return None;
        };
        let index = usize::from(block);
        if index > blocks.len()
            || index >= MAX_BLOCKS
            || blocks
                .get(index)
                .is_some_and(|value| !matches!(value, Block::Text { .. }))
        {
            return None;
        }
        if status == NativeConversationStatus::NeverStarted
            && (*state == ReplyState::Done
                || blocks.iter().any(|block| match block {
                    Block::Text { text, cites } => !text.is_empty() || !cites.is_empty(),
                    _ => true,
                }))
        {
            // A no-send claim cannot erase contradictory provider/effect evidence.
            return None;
        }
        if final_text.is_some()
            || matches!(
                status,
                NativeConversationStatus::Completed
                    | NativeConversationStatus::HistoryUncertain
                    | NativeConversationStatus::NeverStarted
            )
        {
            let text = Block::Text {
                text: final_text.unwrap_or_default(),
                cites,
            };
            if index == blocks.len() {
                blocks.push(text);
            } else {
                blocks[index] = text;
            }
        }
        // An absent final item for Unknown/Interrupted/Failed is not an empty
        // answer. Keep the existing bounded provisional text/cites, with its
        // truthful terminal state below; never promote it to a completed answer.
        let (next_state, next_fault) = match status {
            NativeConversationStatus::Completed => match local_fault {
                Some(fault) => (ReplyState::Failed, Some(fault)),
                None => (ReplyState::Done, None),
            },
            NativeConversationStatus::NeverStarted => (
                ReplyState::Failed,
                Some(match local_fault {
                    Some(NativeFault::Limit) => NativeFault::Limit,
                    Some(NativeFault::Protocol) => NativeFault::Protocol,
                    _ => NativeFault::NotSent,
                }),
            ),
            NativeConversationStatus::HistoryUncertain => {
                (ReplyState::Failed, Some(NativeFault::HistoryUncertain))
            }
            NativeConversationStatus::Interrupted => match local_fault {
                Some(fault) if fault != NativeFault::Cancelled => (ReplyState::Failed, Some(fault)),
                _ => (ReplyState::Cancelled, None),
            },
            NativeConversationStatus::Failed => (ReplyState::Failed, Some(NativeFault::Upstream)),
            NativeConversationStatus::Unknown => {
                (ReplyState::Failed, Some(NativeFault::OutcomeUnknown))
            }
        };
        *state = next_state;
        *phase = None;
        *cost = None;
        *fault = next_fault.map(|fault| Fault::NativeConversation { fault });
        Some(self.changed(reply, Event::Invalidated))
    }

    /// The next block index of `reply`.
    pub fn next_block(&self, reply: u32) -> u16 {
        self.blocks(reply)
            .map_or(0, |blocks| u16::try_from(blocks.len()).unwrap_or(u16::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{PageAssistCostV1 as Cost, PageAssistFaultV1 as Fault};

    fn conversation() -> Conversation {
        Conversation::new("c1".into(), "space".into())
    }

    #[test]
    fn a_message_and_its_reply_fold_their_events_in_order() {
        let mut c = conversation();
        let said = c.say("What's worth reading?", None, Vec::new(), None);
        assert_eq!(said.len(), 2, "the first message names the conversation");
        assert_eq!(c.view().title.as_deref(), Some("What's worth reading?"));
        let (reply, started) = c.begin_reply();
        assert_eq!(started.seq, 3);
        let text = |delta: &str| Event::Text {
            block: 0,
            delta: delta.into(),
        };
        assert!(c.apply(reply, text("Three ")).is_some());
        assert!(c.apply(reply, text("stand out")).is_some());
        assert!(c
            .apply(
                reply,
                Event::Done {
                    cost: Some(Cost::Local { model: "m".into() })
                }
            )
            .is_some());
        let Message::Reply { blocks, state, .. } = &c.messages()[1] else {
            panic!()
        };
        assert_eq!(*state, ReplyState::Done);
        assert_eq!(
            blocks[0],
            Block::Text {
                text: "Three stand out".into(),
                cites: vec![]
            }
        );
        assert_eq!(c.view().seq, 6);
        assert_eq!(c.say("Open all three", None, Vec::new(), None).len(), 1);
    }

    #[test]
    fn blocks_only_append_in_order_and_text_stays_text() {
        let mut c = conversation();
        let (reply, _) = c.begin_reply();
        let gap = Event::Text {
            block: 3,
            delta: "x".into(),
        };
        assert!(c.apply(reply, gap).is_none());
        let paused = Event::Block {
            block: 0,
            value: Block::Paused {
                reason: crate::ipc::PageAssistPauseV1::TookOver,
            },
        };
        assert!(c.apply(reply, paused).is_some());
        let into_paused = Event::Text {
            block: 0,
            delta: "x".into(),
        };
        assert!(c.apply(reply, into_paused).is_none());
        let forged = Event::Block {
            block: 1,
            value: Block::Text {
                text: "x".into(),
                cites: vec![],
            },
        };
        assert!(c.apply(reply, forged).is_none());
        assert!(c.apply(99, Event::Cancelled).is_none());
    }

    #[test]
    fn native_text_snapshot_replaces_and_clears_only_active_bounded_text() {
        let mut c = conversation();
        let (reply, _) = c.begin_reply();
        let snapshot = |text: &str| Event::TextSnapshot {
            block: 0,
            text: text.into(),
        };
        assert!(c.apply(reply, snapshot("provisional")).is_some());
        assert!(c.apply(reply, snapshot("corrected")).is_some());
        assert_eq!(
            c.blocks(reply).unwrap(),
            &[Block::Text {
                text: "corrected".into(),
                cites: vec![]
            }]
        );
        assert!(c
            .apply(reply, snapshot(&"x".repeat(MAX_TEXT_BYTES + 1)))
            .is_none());
        assert!(c.apply(reply + 1, snapshot("wrong reply")).is_none());
        assert!(c
            .apply(
                reply,
                Event::TextSnapshot {
                    block: 2,
                    text: "gap".into()
                }
            )
            .is_none());
        assert!(c.apply(reply, snapshot("")).is_some());
        assert!(c.apply(reply, Event::Done { cost: None }).is_some());
        assert!(c.apply(reply, snapshot("late")).is_none());
        assert_eq!(
            c.blocks(reply).unwrap(),
            &[Block::Text {
                text: String::new(),
                cites: vec![]
            }]
        );
    }

    #[test]
    fn restore_leaves_nothing_running_and_every_reply_inert() {
        let mut c = conversation();
        c.say("hi", None, Vec::new(), None);
        let (reply, _) = c.begin_reply();
        c.apply(
            reply,
            Event::Failed {
                fault: Fault::NoAnswer,
            },
        );
        let (_, _) = c.begin_reply();
        let restored = Conversation::restore(c.view().clone(), "space".into());
        assert!(restored.messages().iter().all(|message| match message {
            Message::Reply { state, inert, .. } => *inert && !matches!(state, ReplyState::Working),
            Message::Person { .. } => true,
        }));
        let mut restored = restored;
        let (next, _) = restored.begin_reply();
        assert_eq!(next, 4);
    }

    #[test]
    fn old_messages_leave_in_pairs() {
        let mut c = conversation();
        for _ in 0..MAX_MESSAGES {
            c.say("q", None, Vec::new(), None);
            c.begin_reply();
        }
        assert!(c.messages().len() <= MAX_MESSAGES);
        assert!(matches!(c.messages()[0], Message::Person { .. }));
    }

    #[test]
    fn inert_marks_earlier_replies_once() {
        let mut c = conversation();
        let (first, _) = c.begin_reply();
        let (second, _) = c.begin_reply();
        assert!(c.apply(first, Event::Inert).is_some());
        assert!(c.apply(first, Event::Inert).is_none());
        assert!(c.apply(second, Event::Inert).is_some());
    }

    fn terminal(status: NativeConversationStatus, text: Option<&str>) -> NativeTerminalProjection {
        NativeTerminalProjection {
            block: 0,
            status,
            final_text: text.map(str::to_owned),
            cites: Vec::new(),
            local_fault: None,
        }
    }

    #[test]
    fn sealed_final_clears_provisional_text_without_reopening_streaming() {
        let mut c = conversation();
        let (reply, _) = c.begin_reply();
        c.apply(
            reply,
            Event::TextSnapshot {
                block: 0,
                text: "provisional".into(),
            },
        )
        .unwrap();
        c.apply(reply, Event::Cancelled).unwrap();
        assert!(c
            .apply(
                reply,
                Event::TextSnapshot {
                    block: 0,
                    text: "late callback".into()
                }
            )
            .is_none());
        let event = c
            .project_native_terminal(reply, terminal(NativeConversationStatus::Completed, None))
            .unwrap();
        assert!(matches!(event.kind, Event::Invalidated));
        assert!(
            matches!(&c.messages()[0], Message::Reply { state: ReplyState::Done, blocks, .. }
            if matches!(&blocks[0], Block::Text { text, .. } if text.is_empty()))
        );
        assert!(c
            .apply(
                reply,
                Event::TextSnapshot {
                    block: 0,
                    text: "still late".into()
                }
            )
            .is_none());
    }

    #[test]
    fn sealed_terminal_keeps_local_interruption_and_unknown_distinct_on_restore() {
        use crate::ipc::{PageAssistFaultV1 as Fault, PageAssistNativeFaultV1 as NativeFault};
        for (status, expected) in [
            (
                NativeConversationStatus::Completed,
                NativeFault::LocalInterruption,
            ),
            (
                NativeConversationStatus::HistoryUncertain,
                NativeFault::HistoryUncertain,
            ),
            (
                NativeConversationStatus::Unknown,
                NativeFault::OutcomeUnknown,
            ),
        ] {
            let mut c = conversation();
            let (reply, _) = c.begin_reply();
            let mut projection = terminal(status, Some("original item"));
            projection.local_fault = Some(NativeFault::LocalInterruption);
            c.project_native_terminal(reply, projection).unwrap();
            let restored = Conversation::restore(c.view().clone(), "space".into());
            assert!(matches!(&restored.messages()[0], Message::Reply {
                state: ReplyState::Failed, fault: Some(Fault::NativeConversation { fault }), blocks, ..
            } if *fault == expected && matches!(&blocks[0], Block::Text { text, .. } if text == "original item")));
        }
    }

    #[test]
    fn malformed_native_terminal_projection_is_atomic_and_bounded() {
        use crate::ipc::PageAssistCiteV1;
        let mut c = conversation();
        let (reply, _) = c.begin_reply();
        c.apply(
            reply,
            Event::TextSnapshot {
                block: 0,
                text: "prior".into(),
            },
        )
        .unwrap();
        for malformed in 0..6 {
            let before = c.clone();
            let mut projection = terminal(NativeConversationStatus::Completed, Some("valid"));
            let target = if malformed == 0 { reply + 1 } else { reply };
            match malformed {
                1 => projection.status = NativeConversationStatus::NeverStarted,
                2 => projection.final_text = Some("x".repeat(MAX_TEXT_BYTES + 1)),
                3 => projection.block = 2,
                4 => {
                    projection.cites = vec![PageAssistCiteV1 {
                        n: 0,
                        tab: "tab".into(),
                        host: "host".into(),
                        excerpt: "quote".into(),
                    }]
                }
                5 => {
                    projection.cites = vec![
                        PageAssistCiteV1 {
                            n: 1,
                            tab: "tab".into(),
                            host: "host".into(),
                            excerpt: "quote".into()
                        };
                        2
                    ]
                }
                _ => {}
            }
            assert!(c.project_native_terminal(target, projection).is_none());
            assert_eq!(c, before);
        }
    }

    #[test]
    fn absent_final_on_unconfirmed_or_failed_turn_preserves_partial_text() {
        for status in [
            NativeConversationStatus::Unknown,
            NativeConversationStatus::Interrupted,
            NativeConversationStatus::Failed,
        ] {
            let mut c = conversation();
            let (reply, _) = c.begin_reply();
            c.apply(
                reply,
                Event::TextSnapshot {
                    block: 0,
                    text: "partial, not final".into(),
                },
            )
            .unwrap();
            c.project_native_terminal(reply, terminal(status, None))
                .unwrap();
            assert!(
                matches!(&c.messages()[0], Message::Reply { state, blocks, .. }
                if *state != ReplyState::Done && matches!(&blocks[0], Block::Text { text, .. } if text == "partial, not final"))
            );
        }
    }

    #[test]
    fn no_send_cannot_erase_prior_provider_text_or_done_evidence() {
        for done in [false, true] {
            let mut c = conversation();
            let (reply, _) = c.begin_reply();
            if done {
                c.apply(reply, Event::Done { cost: None }).unwrap();
            } else {
                c.apply(
                    reply,
                    Event::TextSnapshot {
                        block: 0,
                        text: "provider text".into(),
                    },
                )
                .unwrap();
            }
            let before = c.clone();
            assert!(c
                .project_native_terminal(
                    reply,
                    terminal(NativeConversationStatus::NeverStarted, None)
                )
                .is_none());
            assert_eq!(c, before);
        }
    }

    #[test]
    fn completed_cannot_project_a_contradictory_no_send_or_unknown_local_claim() {
        use crate::ipc::PageAssistNativeFaultV1 as NativeFault;
        for fault in [
            NativeFault::NotSent,
            NativeFault::OutcomeUnknown,
            NativeFault::HistoryUncertain,
            NativeFault::ScopeChanged,
            NativeFault::Cancelled,
        ] {
            let mut c = conversation();
            let (reply, _) = c.begin_reply();
            let before = c.clone();
            let mut projection = terminal(NativeConversationStatus::Completed, Some("known final"));
            projection.local_fault = Some(fault);
            assert!(c.project_native_terminal(reply, projection).is_none());
            assert_eq!(c, before);
        }
    }
}
