//! Recovery projection only. A saved open native reply is not an acknowledged
//! interrupt, and a durable terminal record alone cannot reconstruct its text.
use zephium_core::codex_store::{Association, Dispatch};
use zephium_ipc::page_assist::{
    PageAssistConversationV1, PageAssistFaultV1, PageAssistMessageV1, PageAssistNativeFaultV1,
    PageAssistReplyStateV1,
};
pub(super) fn restore(body: String, association: Option<&Association>) -> Option<String> {
    let Some(association) = association else {
        return Some(body);
    };
    let mut view: PageAssistConversationV1 = serde_json::from_str(&body).ok()?;
    if view.id != association.conversation {
        return None;
    }
    let expected_reply = match association.dispatch {
        Dispatch::Pending { reply, .. }
        | Dispatch::Settled { reply, .. }
        | Dispatch::Skipped { reply } => reply,
        Dispatch::Ready => return Some(body),
    };
    for message in &mut view.messages {
        if let PageAssistMessageV1::Reply {
            id,
            state,
            phase,
            cost,
            fault,
            inert,
            blocks,
            ..
        } = message
        {
            let never_started = matches!(association.dispatch, Dispatch::Skipped { .. });
            if u64::from(*id) == expected_reply
                && never_started
                && (*state == PageAssistReplyStateV1::Done || !blocks.is_empty())
            {
                // Contradictory completed display is not repaired by inventing
                // an original turn or erasing an acknowledged no-send barrier.
                return None;
            }
            if u64::from(*id) == expected_reply
                && (never_started
                    || matches!(
                        state,
                        PageAssistReplyStateV1::Working
                            | PageAssistReplyStateV1::Waiting
                            | PageAssistReplyStateV1::Paused
                    ))
            {
                *state = PageAssistReplyStateV1::Failed;
                *phase = None;
                *cost = None;
                *fault = Some(PageAssistFaultV1::NativeConversation {
                    fault: if never_started {
                        PageAssistNativeFaultV1::NotSent
                    } else {
                        PageAssistNativeFaultV1::OutcomeUnknown
                    },
                });
                *inert = true;
            }
        }
    }
    serde_json::to_string(&view).ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    use zephium_core::codex_store::{Dispatch, Scope, Terminal};
    fn association(dispatch: Dispatch) -> Association {
        Association {
            scope: Scope {
                incarnation: [1; 16],
                grant_epoch: 1,
            },
            conversation: "chat-1".into(),
            thread_id: Some("thread-1".into()),
            revision: 2,
            dispatch,
        }
    }
    fn body(state: PageAssistReplyStateV1) -> String {
        serde_json::to_string(&PageAssistConversationV1 {
            id: "chat-1".into(),
            title: None,
            seq: 3,
            messages: vec![PageAssistMessageV1::Reply {
                id: 2,
                state,
                phase: None,
                blocks: vec![],
                cost: None,
                fault: None,
                inert: false,
            }],
        })
        .unwrap()
    }
    #[test]
    fn pending_restore_is_unknown_not_interrupted() {
        let row = association(Dispatch::Pending {
            reply: 2,
            turn_id: Some("turn-1".into()),
        });
        let out: PageAssistConversationV1 = serde_json::from_str(
            &restore(body(PageAssistReplyStateV1::Working), Some(&row)).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            &out.messages[0],
            PageAssistMessageV1::Reply {
                state: PageAssistReplyStateV1::Failed,
                fault: Some(PageAssistFaultV1::NativeConversation {
                    fault: PageAssistNativeFaultV1::OutcomeUnknown
                }),
                ..
            }
        ));
    }
    #[test]
    fn metadata_terminal_does_not_invent_missing_text() {
        let row = association(Dispatch::Settled {
            reply: 2,
            turn_id: "turn-1".into(),
            outcome: Terminal::Completed,
        });
        let out = restore(body(PageAssistReplyStateV1::Working), Some(&row)).unwrap();
        assert!(out.contains("outcome_unknown"));
        assert!(!out.contains("\"state\":\"done\""));
    }
    #[test]
    fn already_saved_terminal_is_preserved() {
        let row = association(Dispatch::Settled {
            reply: 2,
            turn_id: "turn-1".into(),
            outcome: Terminal::Completed,
        });
        let before = body(PageAssistReplyStateV1::Done);
        assert_eq!(restore(before.clone(), Some(&row)), Some(before));
    }
    #[test]
    fn legacy_and_mismatched_history_are_not_adopted() {
        let before = body(PageAssistReplyStateV1::Working);
        assert_eq!(restore(before.clone(), None), Some(before.clone()));
        let mut row = association(Dispatch::Ready);
        row.conversation = "different".into();
        assert_eq!(restore(before, Some(&row)), None);
    }
    #[test]
    fn unrelated_backend_reply_is_not_reclassified() {
        let row = association(Dispatch::Settled {
            reply: 2,
            turn_id: "turn-1".into(),
            outcome: Terminal::Completed,
        });
        let mut view: PageAssistConversationV1 =
            serde_json::from_str(&body(PageAssistReplyStateV1::Working)).unwrap();
        if let PageAssistMessageV1::Reply { id, .. } = &mut view.messages[0] {
            *id = 4;
        }
        let before = serde_json::to_string(&view).unwrap();
        assert_eq!(restore(before.clone(), Some(&row)), Some(before));
        let ready = association(Dispatch::Ready);
        let before = body(PageAssistReplyStateV1::Working);
        assert_eq!(restore(before.clone(), Some(&ready)), Some(before));
    }
    #[test]
    fn acknowledged_skipped_reply_is_not_sent_not_unknown_or_done() {
        let mut row = association(Dispatch::Skipped { reply: 2 });
        row.thread_id = None;
        for state in [
            PageAssistReplyStateV1::Working,
            PageAssistReplyStateV1::Cancelled,
            PageAssistReplyStateV1::Failed,
        ] {
            let out = restore(body(state), Some(&row)).unwrap();
            let view: PageAssistConversationV1 = serde_json::from_str(&out).unwrap();
            assert!(matches!(
                &view.messages[0],
                PageAssistMessageV1::Reply {
                    state: PageAssistReplyStateV1::Failed,
                    fault: Some(PageAssistFaultV1::NativeConversation {
                        fault: PageAssistNativeFaultV1::NotSent
                    }),
                    cost: None,
                    inert: true,
                    ..
                }
            ));
        }
        assert!(restore(body(PageAssistReplyStateV1::Done), Some(&row)).is_none());
        let mut conflicting: PageAssistConversationV1 =
            serde_json::from_str(&body(PageAssistReplyStateV1::Failed)).unwrap();
        if let PageAssistMessageV1::Reply { blocks, .. } = &mut conflicting.messages[0] {
            blocks.push(zephium_ipc::page_assist::PageAssistBlockV1::Text {
                text: "contradictory native output".into(),
                cites: Vec::new(),
            });
        }
        assert!(restore(serde_json::to_string(&conflicting).unwrap(), Some(&row)).is_none());
    }
    #[test]
    fn skipped_receipt_does_not_change_an_unrelated_visible_reply() {
        let row = association(Dispatch::Skipped { reply: 3 });
        let before = body(PageAssistReplyStateV1::Cancelled);
        assert_eq!(restore(before.clone(), Some(&row)), Some(before));
    }
}
