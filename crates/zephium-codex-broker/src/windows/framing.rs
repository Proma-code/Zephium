//! Bounded JSONL framing only. JSON/schema admission remains in the typed protocol.
use std::collections::VecDeque;

pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUED_FRAMES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameError {
    Empty,
    TooLarge,
    QueueFull,
    Incomplete,
}

#[derive(Default)]
pub(crate) struct Frames {
    partial: Vec<u8>,
    ready: VecDeque<Vec<u8>>,
    queued: usize,
}
impl Frames {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        for byte in bytes {
            if *byte == b'\n' {
                if self.partial.last() == Some(&b'\r') {
                    self.partial.pop();
                }
                if self.partial.is_empty() {
                    return Err(FrameError::Empty);
                }
                let length = self.partial.len();
                let next = self
                    .queued
                    .checked_add(length)
                    .ok_or(FrameError::QueueFull)?;
                if next > MAX_QUEUED_BYTES || self.ready.len() >= MAX_QUEUED_FRAMES {
                    return Err(FrameError::QueueFull);
                }
                self.queued = next;
                self.ready.push_back(std::mem::take(&mut self.partial));
            } else {
                if self.partial.len() >= MAX_FRAME_BYTES {
                    return Err(FrameError::TooLarge);
                }
                self.partial.push(*byte);
            }
        }
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<Vec<u8>> {
        let value = self.ready.pop_front()?;
        self.queued -= value.len();
        Some(value)
    }

    pub(crate) fn eof(&self) -> Result<(), FrameError> {
        if self.partial.is_empty() {
            Ok(())
        } else {
            Err(FrameError::Incomplete)
        }
    }

    pub(crate) fn pending(&self) -> usize {
        self.ready.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_and_coalesced_lines_preserve_exact_bytes() {
        let mut frames = Frames::default();
        frames.push(b"{\"id\"").unwrap();
        assert_eq!(frames.pop(), None);
        frames.push(b":1}\r\n{\"id\":2}\n").unwrap();
        assert_eq!(frames.pop(), Some(br#"{"id":1}"#.to_vec()));
        assert_eq!(frames.pop(), Some(br#"{"id":2}"#.to_vec()));
        assert_eq!(frames.pop(), None);
        assert_eq!(frames.eof(), Ok(()));
    }

    #[test]
    fn empty_incomplete_and_oversize_frames_refuse() {
        assert_eq!(Frames::default().push(b"\n"), Err(FrameError::Empty));
        assert_eq!(Frames::default().push(b"\r\n"), Err(FrameError::Empty));
        let mut frames = Frames::default();
        frames.push(b"{}").unwrap();
        assert_eq!(frames.eof(), Err(FrameError::Incomplete));
        assert_eq!(
            Frames::default().push(&vec![b'x'; MAX_FRAME_BYTES + 1]),
            Err(FrameError::TooLarge)
        );
    }

    #[test]
    fn queue_is_bounded_and_consumption_reclaims_only_its_bytes() {
        let frame = vec![b'x'; MAX_FRAME_BYTES];
        let mut frames = Frames::default();
        for _ in 0..4 {
            frames.push(&frame).unwrap();
            frames.push(b"\n").unwrap();
        }
        assert_eq!(frames.pending(), 4);
        assert_eq!(frames.pop().unwrap().len(), MAX_FRAME_BYTES);
        frames.push(&frame).unwrap();
        frames.push(b"\n").unwrap();
        frames.push(b"x").unwrap();
        assert_eq!(frames.push(b"\n"), Err(FrameError::QueueFull));
    }

    #[test]
    fn tiny_frame_count_is_bounded_independently_of_bytes() {
        let mut frames = Frames::default();
        for _ in 0..MAX_QUEUED_FRAMES {
            frames.push(b"x\n").unwrap();
        }
        assert_eq!(frames.pop(), Some(b"x".to_vec()));
        frames.push(b"y\n").unwrap();
        assert_eq!(frames.pending(), MAX_QUEUED_FRAMES);
        assert_eq!(frames.push(b"z\n"), Err(FrameError::QueueFull));
    }

    #[test]
    fn bare_carriage_return_is_payload_not_a_second_frame() {
        let mut frames = Frames::default();
        frames.push(b"{\r}\n").unwrap();
        assert_eq!(frames.pop(), Some(b"{\r}".to_vec()));
        assert_eq!(frames.eof(), Ok(()));
    }
}
