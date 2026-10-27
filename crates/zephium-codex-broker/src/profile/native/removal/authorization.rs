//! Canonical Store observation is necessary, never sufficient deletion authority.
use super::*;
use zephium_core::ports::store::ProfileDeletionLoad;

/// Installed by the native composition root using the actual canonical Store.
/// It carries no caller-selected path, provider grant, or renderer boolean.
pub trait RemovalJournal: Send + Sync {
    fn pending_profile_deletions(&self) -> ProfileDeletionLoad;
    fn registered_profiles(&self) -> Option<Vec<zephium_core::ids::ProfileId>>;
}
pub(super) fn require_authorized(
    journal: &dyn RemovalJournal,
    profile: ProfileId,
) -> Result<(), RemovalFault> {
    let ProfileDeletionLoad::Loaded(rows) = journal.pending_profile_deletions() else {
        return Err(RemovalFault::Storage);
    };
    if rows.len() > 256 {
        return Err(RemovalFault::Scope);
    }
    let mut profiles = std::collections::BTreeSet::new();
    let mut matched = false;
    for row in rows {
        let bytes = row.profile.bytes();
        if !profiles.insert(bytes) {
            return Err(RemovalFault::Scope);
        }
        if ProfileId::from_native(bytes) == Some(profile) {
            matched = true;
        }
    }
    if matched {
        Ok(())
    } else {
        Err(RemovalFault::Scope)
    }
}

pub(super) fn require_finalized(
    journal: &dyn RemovalJournal,
    profile: ProfileId,
) -> Result<(), RemovalFault> {
    let ProfileDeletionLoad::Loaded(rows) = journal.pending_profile_deletions() else {
        return Err(RemovalFault::Storage);
    };
    if rows.len() > 256 {
        return Err(RemovalFault::Scope);
    }
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        if !seen.insert(row.profile.bytes()) || row.profile.bytes() == profile.bytes() {
            return Err(RemovalFault::Scope);
        }
    }
    let profiles = journal.registered_profiles().ok_or(RemovalFault::Storage)?;
    if profiles.len() > 256 {
        return Err(RemovalFault::Scope);
    }
    seen.clear();
    for value in profiles {
        if !seen.insert(value.bytes()) || value.bytes() == profile.bytes() {
            return Err(RemovalFault::Scope);
        }
    }
    Ok(())
}
