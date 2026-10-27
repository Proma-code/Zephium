//! Final delivery is separate from admission of another model request. This
//! private receipt is minted only by the exact scoped owner worker. Its payload
//! is preserved verbatim: Unknown/HistoryUncertain cannot become Completed.
use zephium_ipc::page_assist::PageAssistNativeFaultV1;

pub(super) enum ReplyDelivery<T> {
    Settled(T),
    Failed(PageAssistNativeFaultV1),
}
impl<T> ReplyDelivery<T> {
    pub(super) fn receive(
        self,
        same_profile_epoch: bool,
        admission_current: bool,
    ) -> Result<T, PageAssistNativeFaultV1> {
        if !same_profile_epoch {
            return Err(PageAssistNativeFaultV1::ScopeChanged);
        }
        match self {
            Self::Settled(outcome) => Ok(outcome),
            Self::Failed(fault) if admission_current => Err(fault),
            Self::Failed(_) => Err(PageAssistNativeFaultV1::ScopeChanged),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_then_owner_fault_does_not_erase_exact_receipt() {
        let receipt = ReplyDelivery::Settled(("completed", "original final text"));
        assert_eq!(
            receipt.receive(true, false).unwrap(),
            ("completed", "original final text")
        );
    }
    #[test]
    fn unknown_and_unsaved_terminal_are_not_promoted_on_retirement() {
        for status in ["unknown", "history_uncertain"] {
            let receipt = ReplyDelivery::Settled((status, Some("original text")));
            assert_eq!(
                receipt.receive(true, false).unwrap(),
                (status, Some("original text"))
            );
        }
    }
    #[test]
    fn profile_epoch_change_still_fences_a_confirmed_delivery() {
        assert_eq!(
            ReplyDelivery::Settled("confirmed text").receive(false, false),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        );
    }
    #[test]
    fn unconfirmed_failure_requires_current_admission() {
        assert_eq!(
            ReplyDelivery::<()>::Failed(PageAssistNativeFaultV1::Protocol).receive(true, false),
            Err(PageAssistNativeFaultV1::ScopeChanged)
        );
        assert_eq!(
            ReplyDelivery::<()>::Failed(PageAssistNativeFaultV1::Protocol).receive(true, true),
            Err(PageAssistNativeFaultV1::Protocol)
        );
    }
}
