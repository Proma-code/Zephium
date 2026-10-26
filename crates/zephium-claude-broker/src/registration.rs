//! Native trial registration format. The checksum detects corruption, not a
//! malicious same-user rewrite; authority comes from the verified native root,
//! private file and caller's actual profile identity, never these bytes alone.
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 16] = b"ZPHCLAUDEREG0001";
const BODY_LEN: usize = 432;
pub(crate) const RECORD_LEN: usize = BODY_LEN + 32;
pub(crate) const NODE_COUNT: usize = 13;
pub(crate) const NAMESPACE: &str = "Zephium.SubscriptionTrial.Claude.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProfileKey {
    pub(crate) profile: [u8; 16],
    pub(crate) incarnation: [u8; 16],
}

/// Explicit policy identity, not masks supplied by a registration file. Native
/// callers must independently admit a policy; decoding never upgrades one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PolicyVersion {
    DataV1,
    FileReplaceV1,
}
impl PolicyVersion {
    fn byte(self) -> u8 {
        match self {
            Self::DataV1 => 1,
            Self::FileReplaceV1 => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NodeIdentity {
    pub(crate) volume: u64,
    pub(crate) file: [u8; 16],
}

/// Fixed role order. The registration node is created before its own identity
/// is encoded; a partial/empty record is never a valid reopen receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub(crate) enum Node {
    Anchor,
    Namespace,
    Root,
    Config,
    Lock,
    State,
    Registration,
    Home,
    AppData,
    Work,
    Scratch,
    OsData,
    OsTemp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Record {
    pub(crate) key: ProfileKey,
    pub(crate) policy: PolicyVersion,
    pub(crate) owner_sid_hash: [u8; 32],
    pub(crate) package_sid_hash: [u8; 32],
    pub(crate) nodes: [NodeIdentity; NODE_COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Invalid,
    WrongProfile,
    WrongOwner,
    WrongPolicy,
}

impl Record {
    pub(crate) fn new(
        key: ProfileKey,
        policy: PolicyVersion,
        owner_sid_hash: [u8; 32],
        package_sid_hash: [u8; 32],
        nodes: [NodeIdentity; NODE_COUNT],
    ) -> Result<Self, Error> {
        // Distinct fixed nodes may not alias one another, even if their content
        // happens to be plausible. Native code separately verifies every ID.
        if nodes
            .iter()
            .enumerate()
            .any(|(index, node)| nodes[..index].contains(node))
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            key,
            policy,
            owner_sid_hash,
            package_sid_hash,
            nodes,
        })
    }

    pub(crate) fn encode(&self) -> [u8; RECORD_LEN] {
        let mut bytes = [0u8; RECORD_LEN];
        bytes[..16].copy_from_slice(MAGIC);
        bytes[16] = self.policy.byte();
        // Bytes 17..24 are reserved and must remain zero.
        bytes[24..40].copy_from_slice(&self.key.profile);
        bytes[40..56].copy_from_slice(&self.key.incarnation);
        bytes[56..88].copy_from_slice(&self.owner_sid_hash);
        bytes[88..120].copy_from_slice(&self.package_sid_hash);
        for (slot, node) in self.nodes.iter().enumerate() {
            let start = 120 + slot * 24;
            bytes[start..start + 8].copy_from_slice(&node.volume.to_le_bytes());
            bytes[start + 8..start + 24].copy_from_slice(&node.file);
        }
        let checksum = Sha256::digest(&bytes[..BODY_LEN]);
        bytes[BODY_LEN..].copy_from_slice(&checksum);
        bytes
    }

    pub(crate) fn decode(
        bytes: &[u8],
        expected_profile: [u8; 16],
        expected_owner_sid_hash: [u8; 32],
        expected_policy: PolicyVersion,
    ) -> Result<Self, Error> {
        if bytes.len() != RECORD_LEN
            || &bytes[..16] != MAGIC
            || bytes[17..24] != [0; 7]
            || Sha256::digest(&bytes[..BODY_LEN])[..] != bytes[BODY_LEN..]
        {
            return Err(Error::Invalid);
        }
        let policy = match bytes[16] {
            1 => PolicyVersion::DataV1,
            2 => PolicyVersion::FileReplaceV1,
            _ => return Err(Error::Invalid),
        };
        if policy != expected_policy {
            return Err(Error::WrongPolicy);
        }
        let profile = bytes[24..40].try_into().map_err(|_| Error::Invalid)?;
        if profile != expected_profile {
            return Err(Error::WrongProfile);
        }
        let owner_sid_hash = bytes[56..88].try_into().map_err(|_| Error::Invalid)?;
        if owner_sid_hash != expected_owner_sid_hash {
            return Err(Error::WrongOwner);
        }
        let nodes = std::array::from_fn(|slot| {
            let start = 120 + slot * 24;
            let mut volume = [0; 8];
            volume.copy_from_slice(&bytes[start..start + 8]);
            let mut file = [0; 16];
            file.copy_from_slice(&bytes[start + 8..start + 24]);
            NodeIdentity {
                volume: u64::from_le_bytes(volume),
                file,
            }
        });
        Self::new(
            ProfileKey {
                profile,
                incarnation: bytes[40..56].try_into().map_err(|_| Error::Invalid)?,
            },
            policy,
            owner_sid_hash,
            bytes[88..120].try_into().map_err(|_| Error::Invalid)?,
            nodes,
        )
    }

    pub(crate) fn node(&self, role: Node) -> NodeIdentity {
        self.nodes[role as usize]
    }
}

/// Constructible only from the native key, not a caller-selected package name.
pub(crate) struct PrincipalName(String);
impl PrincipalName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn principal_name(key: ProfileKey) -> PrincipalName {
    let mut hash = Sha256::new();
    hash.update(b"Zephium.SubscriptionTrial.Claude.Principal.v1\0");
    hash.update(key.profile);
    hash.update(key.incarnation);
    let digest = hash.finalize();
    // Full 256-bit digest, unpadded RFC4648 base32. The fixed namespace plus
    // 52 digits is 63 characters, below the Windows 64-character name bound.
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut name = String::with_capacity(63);
    name.push_str("Zephium.CT.");
    let mut accumulator = 0u32;
    let mut bits = 0u8;
    for byte in digest {
        accumulator = (accumulator << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            name.push(char::from(ALPHABET[((accumulator >> bits) & 31) as usize]));
        }
    }
    if bits != 0 {
        name.push(char::from(
            ALPHABET[((accumulator << (5 - bits)) & 31) as usize],
        ));
    }
    PrincipalName(name)
}

pub(crate) fn sid_hash(canonical_sid: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"Zephium.SubscriptionTrial.Claude.Sid.v1\0");
    hash.update(canonical_sid.as_bytes());
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        Record::new(
            ProfileKey {
                profile: [1; 16],
                incarnation: [2; 16],
            },
            PolicyVersion::DataV1,
            sid_hash("S-1-5-21-1"),
            sid_hash("S-1-15-2-2"),
            std::array::from_fn(|slot| NodeIdentity {
                volume: 7,
                file: [slot as u8; 16],
            }),
        )
        .unwrap()
    }
    fn decode(bytes: &[u8]) -> Result<Record, Error> {
        let expected = record();
        Record::decode(
            bytes,
            expected.key.profile,
            expected.owner_sid_hash,
            expected.policy,
        )
    }
    fn checksum(bytes: &mut [u8; RECORD_LEN]) {
        let hash = Sha256::digest(&bytes[..BODY_LEN]);
        bytes[BODY_LEN..].copy_from_slice(&hash);
    }
    #[test]
    fn exact_roundtrip_preserves_every_fixed_identity() {
        let record = record();
        assert_eq!(decode(&record.encode()), Ok(record.clone()));
        for role in [
            Node::Anchor,
            Node::Namespace,
            Node::Root,
            Node::Config,
            Node::Lock,
            Node::State,
            Node::Registration,
            Node::Home,
            Node::AppData,
            Node::Work,
            Node::Scratch,
            Node::OsData,
            Node::OsTemp,
        ] {
            assert_eq!(record.node(role), record.nodes[role as usize]);
        }
        assert_eq!(NAMESPACE, "Zephium.SubscriptionTrial.Claude.v1");
    }
    #[test]
    fn every_single_byte_corruption_and_partial_length_refuses() {
        let bytes = record().encode();
        for size in 0..RECORD_LEN {
            assert!(decode(&bytes[..size]).is_err());
        }
        let mut longer = bytes.to_vec();
        longer.push(0);
        assert!(decode(&longer).is_err());
        for slot in 0..RECORD_LEN {
            let mut changed = bytes;
            changed[slot] ^= 1;
            assert!(decode(&changed).is_err(), "slot {slot}");
        }
    }
    #[test]
    fn well_checksummed_wrong_scope_policy_version_and_reserved_bytes_refuse() {
        let bytes = record().encode();
        for slot in [0, 15, 17, 23] {
            let mut changed = bytes;
            changed[slot] ^= 1;
            checksum(&mut changed);
            assert_eq!(decode(&changed), Err(Error::Invalid));
        }
        for (slot, error) in [
            (24, Error::WrongProfile),
            (56, Error::WrongOwner),
            (16, Error::WrongPolicy),
        ] {
            let mut changed = bytes;
            changed[slot] = if slot == 16 { 2 } else { changed[slot] ^ 1 };
            checksum(&mut changed);
            assert_eq!(decode(&changed), Err(error));
        }
        let mut unknown = bytes;
        unknown[16] = 255;
        checksum(&mut unknown);
        assert_eq!(decode(&unknown), Err(Error::Invalid));
    }
    #[test]
    fn duplicate_node_aliases_are_not_valid_even_with_correct_checksum() {
        let mut bytes = record().encode();
        let first: [u8; 24] = bytes[120..144].try_into().unwrap();
        bytes[144..168].copy_from_slice(&first);
        checksum(&mut bytes);
        assert_eq!(decode(&bytes), Err(Error::Invalid));
    }
    #[test]
    fn principal_is_bounded_deterministic_and_binds_both_native_ids() {
        let key = record().key;
        let name = principal_name(key);
        // Independent hashlib + RFC4648 base32 reference vector.
        assert_eq!(
            name.as_str(),
            "Zephium.CT.TWDG25QGHIWSVVN2QB33JVI2YMMVGQB3R5GK7WK3TPEE6WUW3BJA"
        );
        assert_eq!(name.as_str().len(), 63);
        assert!(name.as_str().starts_with("Zephium.CT."));
        assert!(name
            .as_str()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.'));
        assert_eq!(name.as_str(), principal_name(key).as_str());
        for changed in [
            ProfileKey {
                profile: [3; 16],
                ..key
            },
            ProfileKey {
                incarnation: [3; 16],
                ..key
            },
        ] {
            assert!(!name
                .as_str()
                .eq_ignore_ascii_case(principal_name(changed).as_str()));
        }
    }
    #[test]
    fn principal_base32_preserves_all_256_hash_bits() {
        let key = record().key;
        let name = principal_name(key);
        let mut decoded = Vec::new();
        let mut accumulator = 0u32;
        let mut bits = 0;
        for byte in name.as_str().strip_prefix("Zephium.CT.").unwrap().bytes() {
            let digit = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
                .iter()
                .position(|c| *c == byte)
                .unwrap();
            accumulator = (accumulator << 5) | digit as u32;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                decoded.push((accumulator >> bits) as u8);
            }
        }
        assert_eq!(bits, 4);
        assert_eq!(accumulator & 15, 0);
        let mut expected = Sha256::new();
        expected.update(b"Zephium.SubscriptionTrial.Claude.Principal.v1\0");
        expected.update(key.profile);
        expected.update(key.incarnation);
        assert_eq!(decoded, expected.finalize().to_vec());
    }
}
