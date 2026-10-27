//! Protected current no-owner/no-home observation. It is not a fabricated
//! configured account registration, epoch, logout, or vault-erasure receipt.
use super::removal::RemovalAttemptId;
use super::{NodeId, ProfileId};
use sha2::{Digest, Sha256};
const MAGIC: &[u8; 8] = b"ZCDABS01";
const BODY: usize = 128;
pub(crate) const ABSENCE_LENGTH: usize = BODY + 32;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AbsencePhase {
    Reserved = 1,
    Prepared = 2,
    Completed = 3,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AbsenceRecord {
    pub(crate) phase: AbsencePhase,
    pub(crate) profile: ProfileId,
    pub(crate) attempt: RemovalAttemptId,
    pub(crate) owner: [u8; 32],
    pub(crate) parent: NodeId,
    pub(crate) journal: NodeId,
}
impl AbsenceRecord {
    pub(crate) fn valid(&self) -> bool {
        self.owner != [0; 32]
            && self.attempt.bytes() != [0; 16]
            && self.parent.file != [0; 16]
            && self.journal.file != [0; 16]
            && self.parent != self.journal
            && self.parent.volume == self.journal.volume
    }
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        let mut bytes = Vec::with_capacity(ABSENCE_LENGTH);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(self.phase as u8);
        bytes.extend_from_slice(&[0; 3]);
        bytes.extend_from_slice(&self.profile.bytes());
        bytes.extend_from_slice(&self.attempt.bytes());
        bytes.extend_from_slice(&self.owner);
        for node in [self.parent, self.journal] {
            bytes.extend_from_slice(&node.volume.to_le_bytes());
            bytes.extend_from_slice(&node.file);
        }
        bytes.extend_from_slice(&Sha256::digest(&bytes));
        Some(bytes)
    }
    pub(crate) fn decode(bytes: &[u8], profile: ProfileId, owner: [u8; 32]) -> Option<Self> {
        if bytes.len() != ABSENCE_LENGTH
            || &bytes[..8] != MAGIC
            || bytes[8..12] != 1u32.to_le_bytes()
            || bytes[13..16] != [0; 3]
            || bytes[16..32] != profile.bytes()
            || bytes[48..80] != owner
            || Sha256::digest(&bytes[..BODY]).as_slice() != &bytes[BODY..]
        {
            return None;
        }
        let node = |offset| {
            Some(NodeId {
                volume: u64::from_le_bytes(bytes[offset..offset + 8].try_into().ok()?),
                file: bytes[offset + 8..offset + 24].try_into().ok()?,
            })
        };
        let value = Self {
            phase: match bytes[12] {
                1 => AbsencePhase::Reserved,
                2 => AbsencePhase::Prepared,
                3 => AbsencePhase::Completed,
                _ => return None,
            },
            profile,
            attempt: RemovalAttemptId::from_native_bytes(bytes[32..48].try_into().ok()?)?,
            owner,
            parent: node(80)?,
            journal: node(104)?,
        };
        value.valid().then_some(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> AbsenceRecord {
        AbsenceRecord {
            phase: AbsencePhase::Reserved,
            profile: ProfileId::from_native([1; 16]).unwrap(),
            attempt: RemovalAttemptId::from_native_bytes([2; 16]).unwrap(),
            owner: [3; 32],
            parent: NodeId {
                volume: 4,
                file: [5; 16],
            },
            journal: NodeId {
                volume: 4,
                file: [6; 16],
            },
        }
    }
    #[test]
    fn absence_record_is_distinct_bounded_exact_and_contains_no_configured_epoch() {
        for phase in [
            AbsencePhase::Reserved,
            AbsencePhase::Prepared,
            AbsencePhase::Completed,
        ] {
            let value = AbsenceRecord { phase, ..sample() };
            let bytes = value.encode().unwrap();
            assert_eq!(bytes.len(), ABSENCE_LENGTH);
            assert_eq!(
                AbsenceRecord::decode(&bytes, value.profile, value.owner),
                Some(value)
            );
        }
    }
    #[test]
    fn malformed_future_wrong_scope_and_aliased_absence_records_refuse() {
        let value = sample();
        let bytes = value.encode().unwrap();
        for index in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[index] ^= 1;
            assert!(AbsenceRecord::decode(&bad, value.profile, value.owner).is_none());
        }
        assert!(
            AbsenceRecord::decode(&bytes[..bytes.len() - 1], value.profile, value.owner).is_none()
        );
        assert!(AbsenceRecord::decode(
            &bytes,
            ProfileId::from_native([9; 16]).unwrap(),
            value.owner
        )
        .is_none());
        let mut alias = value.clone();
        alias.journal = alias.parent;
        assert!(alias.encode().is_none());
        let mut foreign = value;
        foreign.journal.volume = 8;
        assert!(foreign.encode().is_none());
    }
}
