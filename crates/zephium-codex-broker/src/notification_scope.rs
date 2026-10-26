//! Correlation for content-free advisory notifications, never state authority.
//! Original tasks may notify before their RPC reply. Retain at most one pending
//! thread/turn identity, then require the authoritative reply to join it exactly.
use crate::wire::AdvisoryScope;

#[derive(Default)]
pub(crate) struct AdvisoryJoin {
    opening_thread: Option<String>,
    opening_turn: Option<(String, String)>,
}
impl AdvisoryJoin {
    pub(crate) fn accept(
        &mut self,
        scope: AdvisoryScope<'_>,
        owned_threads: &[&str],
        owned_turns: &[(&str, &str)],
        starting_thread: bool,
        starting_turn: Option<&str>,
    ) -> Result<(), ()> {
        match scope {
            AdvisoryScope::Global => Ok(()),
            AdvisoryScope::Thread(thread) if owned_threads.contains(&thread) => Ok(()),
            AdvisoryScope::Thread(thread) if starting_thread => {
                match self.opening_thread.as_deref() {
                    Some(earlier) if earlier != thread => return Err(()),
                    Some(_) => (),
                    None => self.opening_thread = Some(thread.to_owned()),
                }
                Ok(())
            }
            AdvisoryScope::Turn { thread_id, turn_id }
                if owned_turns.contains(&(thread_id, turn_id)) =>
            {
                Ok(())
            }
            AdvisoryScope::Turn { thread_id, turn_id } if starting_turn == Some(thread_id) => {
                match self.opening_turn.as_ref() {
                    Some((thread, turn)) if thread != thread_id || turn != turn_id => {
                        return Err(());
                    }
                    Some(_) => (),
                    None => self.opening_turn = Some((thread_id.to_owned(), turn_id.to_owned())),
                }
                Ok(())
            }
            _ => Err(()),
        }
    }
    pub(crate) fn thread_reply(&mut self, thread: &str) -> Result<(), ()> {
        match self.opening_thread.take() {
            Some(expected) if expected != thread => Err(()),
            _ => Ok(()),
        }
    }
    pub(crate) fn turn_reply(&mut self, thread: &str, turn: &str) -> Result<(), ()> {
        match self.opening_turn.take() {
            Some((expected_thread, expected_turn))
                if expected_thread != thread || expected_turn != turn =>
            {
                Err(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn early_thread_is_pending_correlation_not_new_ownership() {
        let mut join = AdvisoryJoin::default();
        let scope = AdvisoryScope::Thread("owned");
        assert_eq!(join.accept(scope, &[], &[], false, None), Err(()));
        assert_eq!(join.accept(scope, &[], &[], true, None), Ok(()));
        assert_eq!(join.accept(scope, &[], &[], true, None), Ok(()));
        assert_eq!(
            join.accept(AdvisoryScope::Thread("other"), &[], &[], true, None),
            Err(())
        );
        assert_eq!(join.thread_reply("owned"), Ok(()));
        assert_eq!(join.accept(scope, &["owned"], &[], false, None), Ok(()));
    }
    #[test]
    fn wrong_reply_never_adopts_pending_notification_identity() {
        let mut join = AdvisoryJoin::default();
        join.accept(AdvisoryScope::Thread("candidate"), &[], &[], true, None)
            .unwrap();
        assert_eq!(join.thread_reply("different"), Err(()));
        let early = AdvisoryScope::Turn {
            thread_id: "thread",
            turn_id: "candidate",
        };
        join.accept(early, &["thread"], &[], false, Some("thread"))
            .unwrap();
        assert_eq!(join.turn_reply("thread", "different"), Err(()));
    }
    #[test]
    fn early_turn_needs_exact_pending_thread_and_returned_turn() {
        let mut join = AdvisoryJoin::default();
        let early = AdvisoryScope::Turn {
            thread_id: "thread",
            turn_id: "turn",
        };
        assert_eq!(join.accept(early, &["thread"], &[], false, None), Err(()));
        assert_eq!(
            join.accept(early, &["thread"], &[], false, Some("other")),
            Err(())
        );
        join.accept(early, &["thread"], &[], false, Some("thread"))
            .unwrap();
        assert_eq!(join.turn_reply("thread", "turn"), Ok(()));
        assert_eq!(
            join.accept(early, &["thread"], &[("thread", "turn")], false, None),
            Ok(())
        );
    }
    #[test]
    fn retained_terminal_advisory_does_not_require_a_new_active_turn() {
        let mut join = AdvisoryJoin::default();
        assert_eq!(
            join.accept(
                AdvisoryScope::Turn {
                    thread_id: "old",
                    turn_id: "settled"
                },
                &["old"],
                &[("old", "settled")],
                false,
                None
            ),
            Ok(())
        );
        assert_eq!(
            join.accept(
                AdvisoryScope::Turn {
                    thread_id: "old",
                    turn_id: "foreign"
                },
                &["old"],
                &[("old", "settled")],
                false,
                None
            ),
            Err(())
        );
        assert_eq!(
            join.accept(AdvisoryScope::Global, &[], &[], false, None),
            Ok(())
        );
    }
}
