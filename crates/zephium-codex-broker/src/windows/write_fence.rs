//! Owned final writer admission. Cancellation after this final check cannot undo
//! entered OS writes; the caller retains uncertain dispatch and physical custody.
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
#[derive(Clone)]
pub(crate) struct WriteFence {
    cancel: Option<Arc<AtomicBool>>,
    revision: Option<(Arc<AtomicU64>, u64)>,
    additional: Option<(Arc<AtomicBool>, Arc<AtomicU64>, u64)>,
    browser_budget: Option<Arc<dyn crate::browser_port::BrowserBudget>>,
}
impl WriteFence {
    pub(crate) fn login(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel: Some(cancel),
            revision: None,
            additional: None,
            browser_budget: None,
        }
    }
    pub(crate) fn text(cancel: Arc<AtomicBool>, revision: Arc<AtomicU64>, expected: u64) -> Self {
        Self {
            cancel: Some(cancel),
            revision: Some((revision, expected)),
            additional: None,
            browser_budget: None,
        }
    }
    pub(crate) fn preparation(revision: Arc<AtomicU64>, expected: u64) -> Self {
        Self {
            cancel: None,
            revision: Some((revision, expected)),
            additional: None,
            browser_budget: None,
        }
    }
    pub(crate) fn with_browser_budget(
        mut self,
        budget: Arc<dyn crate::browser_port::BrowserBudget>,
    ) -> Option<Self> {
        if self.browser_budget.is_some() {
            return None;
        }
        self.browser_budget = Some(budget);
        Some(self)
    }
    pub(crate) fn is_text(&self) -> bool {
        self.cancel.is_some() && self.revision.is_some()
    }
    /// Conjunction with a distinct native lifecycle action, never replacement
    /// of the already-owned call/profile fence. Exactly two gates are bounded.
    pub(crate) fn and_action(mut self, action: &Self) -> Option<Self> {
        if self.additional.is_some()
            || action.additional.is_some()
            || action.browser_budget.is_some()
        {
            return None;
        }
        let cancel = action.cancel.as_ref()?.clone();
        let (generation, expected) = action.revision.as_ref()?;
        self.additional = Some((cancel, generation.clone(), *expected));
        Some(self)
    }
    pub(crate) fn same_action(&self, other: &Self) -> bool {
        if self.additional.is_some()
            || other.additional.is_some()
            || self.browser_budget.is_some()
            || other.browser_budget.is_some()
        {
            return false;
        }
        match (&self.cancel, &other.cancel, &self.revision, &other.revision) {
            (Some(a), Some(b), Some((ar, av)), Some((br, bv))) => {
                Arc::ptr_eq(a, b) && Arc::ptr_eq(ar, br) && av == bv
            }
            _ => false,
        }
    }
    pub(crate) fn current(&self) -> bool {
        self.browser_budget.as_ref().is_none_or(|budget| {
            budget
                .deadline()
                .is_ok_and(|deadline| std::time::Instant::now() < deadline)
        }) && self
            .cancel
            .as_ref()
            .is_none_or(|cancel| !cancel.load(Ordering::Acquire))
            && self
                .revision
                .as_ref()
                .is_none_or(|(revision, expected)| revision.load(Ordering::Acquire) == *expected)
            && self
                .additional
                .as_ref()
                .is_none_or(|(cancel, generation, expected)| {
                    !cancel.load(Ordering::Acquire)
                        && generation.load(Ordering::Acquire) == *expected
                })
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn lifecycle_fence_conjoins_without_replacing_original_call_guard() {
        let original = Arc::new(AtomicBool::new(false));
        let action = Arc::new(AtomicBool::new(false));
        let g = Arc::new(AtomicU64::new(1));
        let a = WriteFence::text(action.clone(), g.clone(), 1);
        let both = WriteFence::login(original.clone()).and_action(&a).unwrap();
        assert!(both.current());
        assert!(!both.same_action(&a));
        original.store(true, Ordering::Release);
        assert!(!both.current());
        let original2 = Arc::new(AtomicBool::new(false));
        let both2 = WriteFence::login(original2).and_action(&a).unwrap();
        action.store(true, Ordering::Release);
        assert!(!both2.current());
        assert!(both.and_action(&a).is_none());
    }
    #[test]
    fn same_action_requires_exact_owned_cancel_generation_and_expected_value() {
        let c = Arc::new(AtomicBool::new(false));
        let g = Arc::new(AtomicU64::new(1));
        let a = WriteFence::text(c.clone(), g.clone(), 1);
        assert!(a.same_action(&a.clone()));
        assert!(!a.same_action(&WriteFence::text(
            Arc::new(AtomicBool::new(false)),
            g.clone(),
            1
        )));
        assert!(!a.same_action(&WriteFence::text(c.clone(), Arc::new(AtomicU64::new(1)), 1)));
        assert!(!a.same_action(&WriteFence::text(c.clone(), g.clone(), 2)));
        assert!(!a.same_action(&WriteFence::login(c)));
        g.store(2, Ordering::Release);
        assert!(!a.current());
    }
}
