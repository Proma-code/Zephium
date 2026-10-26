//! Owned final writer admission. Cancellation after this final check cannot undo
//! entered OS writes; the caller retains uncertain dispatch and physical custody.
use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
#[derive(Clone)]
pub(crate) struct WriteFence {
    cancel: Arc<AtomicBool>,
    revision: Option<(Arc<AtomicU64>, u64)>,
}
impl WriteFence {
    pub(crate) fn login(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            revision: None,
        }
    }
    pub(crate) fn text(cancel: Arc<AtomicBool>, revision: Arc<AtomicU64>, expected: u64) -> Self {
        Self {
            cancel,
            revision: Some((revision, expected)),
        }
    }
    pub(crate) fn current(&self) -> bool {
        !self.cancel.load(Ordering::Acquire)
            && self
                .revision
                .as_ref()
                .is_none_or(|(revision, expected)| revision.load(Ordering::Acquire) == *expected)
    }
}
pub(crate) fn write_frame(
    writer: &mut impl Write,
    bytes: &[u8],
    fence: Option<&WriteFence>,
    deadline: Instant,
) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native write admission expired",
        ));
    }
    if fence.is_some_and(|fence| !fence.current()) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "native write admission retired",
        ));
    }
    writer.write_all(bytes)?;
    writer.flush()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_cancel_and_revision_change_write_no_bytes() {
        for changed_revision in [false, true] {
            let cancel = Arc::new(AtomicBool::new(false));
            let revision = Arc::new(AtomicU64::new(7));
            let fence = WriteFence::text(cancel.clone(), revision.clone(), 7);
            let (queued, received) = std::sync::mpsc::sync_channel(1);
            queued.send(fence).unwrap();
            if changed_revision {
                revision.store(8, Ordering::Release);
            } else {
                cancel.store(true, Ordering::Release);
            }
            let mut bytes = Vec::new();
            assert_eq!(
                write_frame(
                    &mut bytes,
                    b"fixed\n",
                    Some(&received.recv().unwrap()),
                    Instant::now() + std::time::Duration::from_secs(1)
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::Interrupted
            );
            assert!(bytes.is_empty());
        }
    }
    #[test]
    fn queued_expired_frame_writes_no_bytes_even_without_optional_fence() {
        let deadline = Instant::now();
        let mut bytes = Vec::new();
        assert_eq!(
            write_frame(&mut bytes, b"expired\n", None, deadline)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(bytes.is_empty());
    }
    #[test]
    fn current_text_and_login_admit_only_the_owned_frame() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut bytes = Vec::new();
        let text = WriteFence::text(cancel.clone(), Arc::new(AtomicU64::new(7)), 7);
        write_frame(
            &mut bytes,
            b"text\n",
            Some(&text),
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .unwrap();
        let login = WriteFence::login(cancel.clone());
        write_frame(
            &mut bytes,
            b"login\n",
            Some(&login),
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .unwrap();
        cancel.store(true, Ordering::Release);
        assert!(write_frame(
            &mut bytes,
            b"late\n",
            Some(&login),
            Instant::now() + std::time::Duration::from_secs(1)
        )
        .is_err());
        assert_eq!(bytes, b"text\nlogin\n");
    }
}
