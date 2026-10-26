//! Native-owned trial profile registration. This records ownership/continuity,
//! never user consent, current account authority, or a completed model outcome.
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"ZCODX001";
const VERSION: u32 = 2;
const LEGACY_VERSION: u32 = 1;
const ROLES: usize = 8;
const BODY: usize = 8 + 4 + 16 + 16 + 32 + ROLES * 24 + 32 + 32;
const LENGTH: usize = BODY + 32;
pub(crate) const CLEAN: &[u8; 8] = b"ZCLEAN01";
pub(crate) const DIRTY: &[u8; 8] = b"ZDIRTY01";

/// A native profile identity is correlation, not renderer-granted authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileId([u8; 16]);
impl ProfileId {
    /// Call only after the native foreground actor admits the selected profile.
    pub fn from_native(bytes: [u8; 16]) -> Option<Self> {
        (bytes != [0; 16]).then_some(Self(bytes))
    }
    pub(crate) fn leaf(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NodeId {
    pub(crate) volume: u64,
    pub(crate) file: [u8; 16],
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Record {
    pub(crate) version: u32,
    pub(crate) profile: ProfileId,
    pub(crate) incarnation: [u8; 16],
    pub(crate) owner: [u8; 32],
    pub(crate) nodes: [NodeId; ROLES],
    pub(crate) config: [u8; 32],
    pub(crate) environments: [u8; 32],
}
impl Record {
    fn valid(&self) -> bool {
        matches!(self.version, LEGACY_VERSION | VERSION)
            && self.incarnation != [0; 16]
            && self.owner != [0; 32]
            && self.config != [0; 32]
            && self.environments != [0; 32]
            && self.nodes.iter().all(|id| id.file != [0; 16])
            && self
                .nodes
                .iter()
                .enumerate()
                .all(|(index, id)| self.nodes.iter().take(index).all(|other| other != id))
    }
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        let mut out = Vec::with_capacity(LENGTH);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.profile.0);
        out.extend_from_slice(&self.incarnation);
        out.extend_from_slice(&self.owner);
        for node in self.nodes {
            out.extend_from_slice(&node.volume.to_le_bytes());
            out.extend_from_slice(&node.file);
        }
        out.extend_from_slice(&self.config);
        out.extend_from_slice(&self.environments);
        let digest = Sha256::digest(&out);
        out.extend_from_slice(&digest);
        Some(out)
    }
    pub(crate) fn decode(bytes: &[u8], expected: ProfileId, owner: [u8; 32]) -> Option<Self> {
        if bytes.len() != LENGTH
            || &bytes[..8] != MAGIC
            || !matches!(
                u32::from_le_bytes(bytes[8..12].try_into().ok()?),
                LEGACY_VERSION | VERSION
            )
            || Sha256::digest(&bytes[..BODY]).as_slice() != &bytes[BODY..]
        {
            return None;
        }
        let profile = ProfileId::from_native(bytes[12..28].try_into().ok()?)?;
        if profile != expected || bytes[44..76] != owner {
            return None;
        }
        let mut nodes = [NodeId {
            volume: 0,
            file: [0; 16],
        }; ROLES];
        for (index, node) in nodes.iter_mut().enumerate() {
            let offset = 76 + index * 24;
            *node = NodeId {
                volume: u64::from_le_bytes(bytes[offset..offset + 8].try_into().ok()?),
                file: bytes[offset + 8..offset + 24].try_into().ok()?,
            };
        }
        let value = Self {
            version: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            profile,
            incarnation: bytes[28..44].try_into().ok()?,
            owner,
            nodes,
            config: bytes[268..300].try_into().ok()?,
            environments: bytes[300..332].try_into().ok()?,
        };
        value.valid().then_some(value)
    }
}

/// Exact legacy bytes retain their V1 meaning. V2 explicitly records the
/// reviewed managed policy, which is independent of the selected runtime.
pub(crate) fn legacy_config_hash() -> [u8; 32] {
    Sha256::digest(crate::config::CONFIG_TOML.as_bytes()).into()
}
pub(crate) fn managed_config() -> std::borrow::Cow<'static, str> {
    crate::compatibility::managed_config()
}
pub(crate) fn managed_policy_hash() -> [u8; 32] {
    crate::compatibility::managed_policy_digest()
}

impl Record {
    pub(crate) fn policy_valid(&self) -> bool {
        let expected = match self.version {
            LEGACY_VERSION => legacy_config_hash(),
            VERSION => managed_policy_hash(),
            _ => return false,
        };
        self.config == expected
            && self.environments
                == <[u8; 32]>::from(Sha256::digest(crate::config::ENVIRONMENTS_TOML.as_bytes()))
    }
    pub(crate) fn managed(&self) -> Self {
        let mut new = self.clone();
        new.version = VERSION;
        new.config = managed_policy_hash();
        new
    }
    pub(crate) fn legacy(&self) -> Self {
        let mut old = self.clone();
        old.version = LEGACY_VERSION;
        old.config = legacy_config_hash();
        old
    }
}
const MIGRATION_MAGIC: &[u8; 8] = b"ZCMIG001";
const MIGRATION_BODY: usize = 8 + 16 + 16 + 32 * 4;
pub(crate) const MIGRATION_LENGTH: usize = MIGRATION_BODY + 32;
/// One exact V1→V2 metadata transition. This never describes child settlement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Migration {
    profile: ProfileId,
    incarnation: [u8; 16],
    old_config: [u8; 32],
    new_config: [u8; 32],
    old_record: [u8; 32],
    new_record: [u8; 32],
}
impl Migration {
    pub(crate) fn for_record(record: &Record) -> Option<Self> {
        if !record.valid() || !record.policy_valid() {
            return None;
        }
        let old = record.legacy();
        let new = record.managed();
        Some(Self {
            profile: record.profile,
            incarnation: record.incarnation,
            old_config: legacy_config_hash(),
            new_config: Sha256::digest(managed_config().as_bytes()).into(),
            old_record: Sha256::digest(old.encode()?).into(),
            new_record: Sha256::digest(new.encode()?).into(),
        })
    }
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MIGRATION_LENGTH);
        out.extend_from_slice(MIGRATION_MAGIC);
        out.extend_from_slice(&self.profile.0);
        out.extend_from_slice(&self.incarnation);
        for hash in [
            self.old_config,
            self.new_config,
            self.old_record,
            self.new_record,
        ] {
            out.extend_from_slice(&hash)
        }
        out.extend_from_slice(&Sha256::digest(&out));
        out
    }
    pub(crate) fn admit(bytes: &[u8], record: &Record) -> Option<Self> {
        let expected = Self::for_record(record)?;
        (bytes == expected.encode()).then_some(expected)
    }
    pub(crate) fn accepts_config(&self, bytes: &[u8]) -> bool {
        let hash: [u8; 32] = Sha256::digest(bytes).into();
        hash == self.old_config || hash == self.new_config
    }
}

#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
pub(crate) mod native;

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Record {
        Record {
            version: VERSION,
            profile: ProfileId::from_native([1; 16]).unwrap(),
            incarnation: [2; 16],
            owner: [3; 32],
            nodes: std::array::from_fn(|i| NodeId {
                volume: 7,
                file: [(i + 1) as u8; 16],
            }),
            config: [4; 32],
            environments: [5; 32],
        }
    }
    #[test]
    fn exact_record_roundtrip_and_native_scope_binding() {
        let value = sample();
        let bytes = value.encode().unwrap();
        assert_eq!(bytes.len(), LENGTH);
        assert_eq!(
            Record::decode(&bytes, value.profile, value.owner),
            Some(value.clone())
        );
        assert!(Record::decode(
            &bytes,
            ProfileId::from_native([9; 16]).unwrap(),
            value.owner
        )
        .is_none());
        assert!(Record::decode(&bytes, value.profile, [9; 32]).is_none());
        assert_eq!(value.profile.leaf(), "01".repeat(16));
    }
    #[test]
    fn corruption_trailing_bytes_and_versions_never_migrate_implicitly() {
        let value = sample();
        let bytes = value.encode().unwrap();
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 1;
            assert!(Record::decode(&bad, value.profile, value.owner).is_none());
        }
        let mut bad = bytes.clone();
        bad.push(0);
        assert!(Record::decode(&bad, value.profile, value.owner).is_none());
        assert!(Record::decode(&bytes[..bytes.len() - 1], value.profile, value.owner).is_none());
        let mut future = bytes;
        future[8] = 3;
        let digest = Sha256::digest(&future[..BODY]);
        future[BODY..].copy_from_slice(&digest);
        assert!(Record::decode(&future, value.profile, value.owner).is_none());
    }
    #[test]
    fn role_alias_empty_identity_and_empty_incarnation_are_rejected() {
        let mut value = sample();
        value.nodes[1] = value.nodes[0];
        assert!(value.encode().is_none());
        let mut value = sample();
        value.nodes[0].file = [0; 16];
        assert!(value.encode().is_none());
        let mut value = sample();
        value.incarnation = [0; 16];
        assert!(value.encode().is_none());
        assert!(ProfileId::from_native([0; 16]).is_none());
        assert_ne!(CLEAN, DIRTY);
    }
    fn legacy_policy_record() -> Record {
        let mut value = sample();
        value.version = LEGACY_VERSION;
        value.config = legacy_config_hash();
        value.environments = Sha256::digest(crate::config::ENVIRONMENTS_TOML.as_bytes()).into();
        value
    }
    #[test]
    fn migration_journal_binds_exact_old_new_policy_records_and_stable_identity() {
        let old = legacy_policy_record();
        let new = old.managed();
        assert!(old.policy_valid());
        assert!(new.policy_valid());
        assert_ne!(old.version, new.version);
        assert_ne!(old.config, new.config);
        assert_eq!(old.profile, new.profile);
        assert_eq!(old.incarnation, new.incarnation);
        assert_eq!(old.owner, new.owner);
        assert_eq!(old.nodes, new.nodes);
        assert_eq!(old.environments, new.environments);
        let migration = Migration::for_record(&old).unwrap();
        let bytes = migration.encode();
        assert_eq!(bytes.len(), MIGRATION_LENGTH);
        assert_eq!(Migration::admit(&bytes, &old), Some(migration.clone()));
        assert_eq!(Migration::admit(&bytes, &new), Some(migration.clone()));
        for config in [
            crate::config::CONFIG_TOML.as_bytes(),
            managed_config().as_bytes(),
        ] {
            assert!(migration.accepts_config(config));
        }
        assert!(!migration.accepts_config(b"unknown configuration"));
        let mut other = old.clone();
        other.incarnation = [9; 16];
        assert!(Migration::admit(&bytes, &other).is_none());
        let mut other = old.clone();
        other.nodes[0].file = [9; 16];
        assert!(Migration::admit(&bytes, &other).is_none());
    }
    #[test]
    fn malformed_torn_journal_and_child_dirty_never_grant_migration_recovery() {
        let old = legacy_policy_record();
        let bytes = Migration::for_record(&old).unwrap().encode();
        for index in 0..bytes.len() {
            let mut corrupted = bytes.clone();
            corrupted[index] ^= 1;
            assert!(Migration::admit(&corrupted, &old).is_none());
        }
        for invalid in [
            CLEAN.as_slice(),
            DIRTY.as_slice(),
            &bytes[..bytes.len() - 1],
        ] {
            assert!(Migration::admit(invalid, &old).is_none());
        }
        let mut unknown = old.clone();
        unknown.config = [17; 32];
        assert!(!unknown.policy_valid());
        assert!(Migration::for_record(&unknown).is_none());
        let mut future = old;
        future.version = 3;
        assert!(future.encode().is_none());
    }
}
