//! Nonsecret protected removal-journal codec. Decoding historical state is not
//! a live erasure capability or proof that an original child has settled.
use super::{NodeId, ProfileId, Record, LENGTH};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"ZCDREM01";
const VERSION: u32 = 1;
const RECORD_OFFSET: usize = 48;
const NODE_OFFSET: usize = RECORD_OFFSET + LENGTH;
const BODY: usize = NODE_OFFSET + 48;
pub(crate) const JOURNAL_LENGTH: usize = BODY + 32;

/// Native-generated correlation nonce, never a renderer authorization token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemovalAttemptId([u8; 16]);
impl RemovalAttemptId {
    pub(crate) fn from_native_bytes(bytes: [u8; 16]) -> Option<Self> {
        (bytes != [0; 16]).then_some(Self(bytes))
    }
    pub(crate) fn bytes(self) -> [u8; 16] {
        self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Reserved = 1,
    Prepared = 2,
    Cleanup = 3,
    RootRemoving = 4,
    Completed = 5,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Journal {
    pub(crate) phase: Phase,
    pub(crate) attempt: RemovalAttemptId,
    pub(crate) reserved_epoch: u64,
    /// Actual acknowledged retirement, not a guessed value after lost ACK.
    pub(crate) retired_epoch: u64,
    pub(crate) registration: Record,
    pub(crate) parent: NodeId,
    pub(crate) journal: NodeId,
}
impl Journal {
    pub(crate) fn reserved(
        attempt: RemovalAttemptId,
        reserved_epoch: u64,
        registration: Record,
        parent: NodeId,
        journal: NodeId,
    ) -> Option<Self> {
        let value = Self {
            phase: Phase::Reserved,
            attempt,
            reserved_epoch,
            retired_epoch: 0,
            registration,
            parent,
            journal,
        };
        value.valid().then_some(value)
    }
    pub(crate) fn valid(&self) -> bool {
        let registered = &self.registration;
        self.attempt.0 != [0; 16]
            && (1..i64::MAX as u64).contains(&self.reserved_epoch)
            && match self.phase {
                Phase::Reserved => self.retired_epoch == 0,
                Phase::Prepared | Phase::Cleanup | Phase::RootRemoving | Phase::Completed => {
                    self.reserved_epoch.checked_add(1) == Some(self.retired_epoch)
                }
            }
            && registered.valid()
            && registered.policy_valid()
            && self.parent.file != [0; 16]
            && self.journal.file != [0; 16]
            && self.parent != self.journal
            && !registered.nodes.contains(&self.parent)
            && !registered.nodes.contains(&self.journal)
            && registered
                .nodes
                .iter()
                .all(|node| node.volume == self.parent.volume)
            && self.journal.volume == self.parent.volume
    }
    /// Serialization-only transition. The native owner must separately consume
    /// exact SettledRemoval custody before it may write these Prepared bytes.
    pub(crate) fn prepared(&self, acknowledged_epoch: u64) -> Option<Self> {
        if self.phase != Phase::Reserved || !self.valid() {
            return None;
        }
        let mut next = self.clone();
        next.phase = Phase::Prepared;
        next.retired_epoch = acknowledged_epoch;
        next.valid().then_some(next)
    }
    /// Only the native owner with the exact authorized app journal/capability
    /// may persist this transition. A parsed record alone cannot authorize it.
    #[cfg(test)]
    pub(crate) fn cleanup(&self) -> Option<Self> {
        if self.phase != Phase::Prepared || !self.valid() {
            return None;
        }
        let mut next = self.clone();
        next.phase = Phase::Cleanup;
        Some(next)
    }
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        let mut bytes = Vec::with_capacity(JOURNAL_LENGTH);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.push(self.phase as u8);
        bytes.extend_from_slice(&[0; 3]);
        bytes.extend_from_slice(&self.attempt.bytes());
        bytes.extend_from_slice(&self.reserved_epoch.to_le_bytes());
        bytes.extend_from_slice(&self.retired_epoch.to_le_bytes());
        bytes.extend_from_slice(&self.registration.encode()?);
        for node in [self.parent, self.journal] {
            bytes.extend_from_slice(&node.volume.to_le_bytes());
            bytes.extend_from_slice(&node.file);
        }
        bytes.extend_from_slice(&Sha256::digest(&bytes));
        Some(bytes)
    }
    pub(crate) fn decode(bytes: &[u8], profile: ProfileId, owner: [u8; 32]) -> Option<Self> {
        if bytes.len() != JOURNAL_LENGTH
            || &bytes[..8] != MAGIC
            || u32::from_le_bytes(bytes[8..12].try_into().ok()?) != VERSION
            || bytes[13..16] != [0; 3]
            || Sha256::digest(&bytes[..BODY]).as_slice() != &bytes[BODY..]
        {
            return None;
        }
        let phase = match bytes[12] {
            1 => Phase::Reserved,
            2 => Phase::Prepared,
            3 => Phase::Cleanup,
            4 => Phase::RootRemoving,
            5 => Phase::Completed,
            _ => return None,
        };
        let node = |offset: usize| -> Option<NodeId> {
            Some(NodeId {
                volume: u64::from_le_bytes(bytes[offset..offset + 8].try_into().ok()?),
                file: bytes[offset + 8..offset + 24].try_into().ok()?,
            })
        };
        let value = Self {
            phase,
            attempt: RemovalAttemptId::from_native_bytes(bytes[16..32].try_into().ok()?)?,
            reserved_epoch: u64::from_le_bytes(bytes[32..40].try_into().ok()?),
            retired_epoch: u64::from_le_bytes(bytes[40..48].try_into().ok()?),
            registration: Record::decode(&bytes[RECORD_OFFSET..NODE_OFFSET], profile, owner)?,
            parent: node(NODE_OFFSET)?,
            journal: node(NODE_OFFSET + 24)?,
        };
        value.valid().then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reserved() -> Journal {
        let registration = Record {
            version: super::super::VERSION,
            profile: ProfileId::from_native([1; 16]).unwrap(),
            incarnation: [2; 16],
            owner: [3; 32],
            nodes: std::array::from_fn(|index| NodeId {
                volume: 7,
                file: [index as u8 + 1; 16],
            }),
            config: super::super::managed_policy_hash(),
            environments: Sha256::digest(crate::config::ENVIRONMENTS_TOML.as_bytes()).into(),
        };
        Journal::reserved(
            RemovalAttemptId::from_native_bytes([4; 16]).unwrap(),
            9,
            registration,
            NodeId {
                volume: 7,
                file: [9; 16],
            },
            NodeId {
                volume: 7,
                file: [10; 16],
            },
        )
        .unwrap()
    }
    fn roundtrip(value: &Journal) {
        let bytes = value.encode().unwrap();
        assert_eq!(bytes.len(), JOURNAL_LENGTH);
        assert_eq!(
            Journal::decode(&bytes, value.registration.profile, value.registration.owner),
            Some(value.clone())
        );
    }
    #[test]
    fn exact_five_phases_preserve_all_identity_and_require_acknowledged_single_retirement() {
        let initial = reserved();
        roundtrip(&initial);
        for epoch in [0, 9, 11, u64::MAX] {
            assert!(initial.prepared(epoch).is_none());
        }
        assert!(initial.cleanup().is_none());
        let prepared = initial.prepared(10).unwrap();
        roundtrip(&prepared);
        assert!(prepared.prepared(11).is_none());
        let cleanup = prepared.cleanup().unwrap();
        roundtrip(&cleanup);
        assert!(cleanup.cleanup().is_none());
        for phase in [Phase::RootRemoving, Phase::Completed] {
            let mut final_phase = cleanup.clone();
            final_phase.phase = phase;
            roundtrip(&final_phase);
            assert_eq!(final_phase.registration, initial.registration);
            assert_eq!(final_phase.retired_epoch, 10);
        }
        assert_eq!(cleanup.registration, initial.registration);
        assert_eq!(cleanup.attempt, initial.attempt);
        assert_eq!(cleanup.parent, initial.parent);
        assert_eq!(cleanup.journal, initial.journal);
    }
    #[test]
    fn malformed_torn_future_or_wrong_profile_owner_never_decodes() {
        let value = reserved();
        let bytes = value.encode().unwrap();
        for offset in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[offset] ^= 1;
            assert!(
                Journal::decode(&bad, value.registration.profile, value.registration.owner)
                    .is_none()
            );
        }
        for length in [0, 8, JOURNAL_LENGTH - 1, JOURNAL_LENGTH + 1] {
            let mut bad = bytes.clone();
            bad.resize(length, 0);
            assert!(
                Journal::decode(&bad, value.registration.profile, value.registration.owner)
                    .is_none()
            );
        }
        for (offset, value_byte) in [(8, 2), (12, 6), (13, 1)] {
            let mut bad = bytes.clone();
            bad[offset] = value_byte;
            let digest = Sha256::digest(&bad[..BODY]);
            bad[BODY..].copy_from_slice(&digest);
            assert!(
                Journal::decode(&bad, value.registration.profile, value.registration.owner)
                    .is_none()
            );
        }
        assert!(Journal::decode(
            &bytes,
            ProfileId::from_native([99; 16]).unwrap(),
            value.registration.owner
        )
        .is_none());
        assert!(Journal::decode(&bytes, value.registration.profile, [99; 32]).is_none());
    }
    #[test]
    fn alias_cross_volume_zero_nonce_and_epoch_exhaustion_are_invalid() {
        let value = reserved();
        assert!(RemovalAttemptId::from_native_bytes([0; 16]).is_none());
        for epoch in [0, i64::MAX as u64, u64::MAX] {
            let mut bad = value.clone();
            bad.reserved_epoch = epoch;
            assert!(bad.encode().is_none());
        }
        for node in [
            value.parent,
            value.registration.nodes[0],
            NodeId {
                volume: 8,
                file: [20; 16],
            },
            NodeId {
                volume: 7,
                file: [0; 16],
            },
        ] {
            let mut bad = value.clone();
            bad.journal = node;
            assert!(bad.encode().is_none());
        }
        let mut bad = value.clone();
        bad.registration.nodes[2].volume = 8;
        assert!(bad.encode().is_none());
        let mut bad = value.clone();
        bad.retired_epoch = 10;
        assert!(bad.encode().is_none());
        let mut bad = value.prepared(10).unwrap();
        bad.retired_epoch = 0;
        assert!(bad.encode().is_none());
    }
}
