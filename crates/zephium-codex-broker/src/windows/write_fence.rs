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
}
impl WriteFence {
    pub(crate) fn login(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel: Some(cancel),
            revision: None,
        }
    }
    pub(crate) fn text(cancel: Arc<AtomicBool>, revision: Arc<AtomicU64>, expected: u64) -> Self {
        Self {
            cancel: Some(cancel),
            revision: Some((revision, expected)),
        }
    }
    pub(crate) fn preparation(revision: Arc<AtomicU64>, expected: u64) -> Self {
        Self {
            cancel: None,
            revision: Some((revision, expected)),
        }
    }
    pub(crate) fn is_text(&self) -> bool {
        self.cancel.is_some() && self.revision.is_some()
    }
    pub(crate) fn current(&self) -> bool {
        self.cancel
            .as_ref()
            .is_none_or(|cancel| !cancel.load(Ordering::Acquire))
            && self
                .revision
                .as_ref()
                .is_none_or(|(revision, expected)| revision.load(Ordering::Acquire) == *expected)
    }
}
