//! Crash-resumable profile-deletion authorization and native erasure.

use super::*;
use crate::profile_removal::{
    AuthorizedRemoval, CleanupProgress, ExplicitRemovalRetry, FinalizedRemoval, InventoryStatus,
    Preparation, PreparedKind, PreparedRemoval, PreparedStatus, ProfileRemovalParticipant,
    RecoverableRemoval, RecoveredRemovalStage, RemovalAttempt, RemovalStage,
};

pub(super) const PROFILE_DELETION_STORE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);
const PROFILE_DELETION_RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(250);
const PROFILE_DELETION_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) enum ProfileDeletionPhase {
    /// Recovery or exact native interruption preserves intent/quarantine but
    /// cannot authorize active-profile deletion without explicit reconciliation.
    AwaitingExplicitRetry,
    /// Original logout and physical settlement must precede removal from the
    /// Store's active registry. No engine or profile data has been erased.
    PreparingNative,
    /// Durable deletion authorization exists. Promote the exact retained
    /// native capability (or recover protected intent) before engine erasure.
    CleanupReady {
        engine_verified: bool,
    },
    /// No Store deletion authority exists yet. The next action builds a
    /// fresh post-removal snapshot and authorizes it.
    AwaitingAuthorization,
    /// Authorization RPC entered the storage actor but its result missed the
    /// caller deadline. An ordered journal read must resolve that attempt
    /// before a retry builds a fresh post-removal snapshot from the current
    /// aggregates. Retaining the original snapshot here would let a later
    /// retry overwrite survivor mutations accepted in the meantime.
    Authorizing {
        may_reauthorize: bool,
    },
    /// `AlreadyAuthorized` proved the barrier, but an exact journal read is
    /// still needed to choose native erasure versus local-only finalization.
    ResolveAuthorizedJournal,
    NativeReady,
    NativeInFlight {
        attempt: u64,
    },
    FinalizeReady,
    NativeFinalizationReady,
    /// A Store/application invariant failed closed. The durable journal and
    /// native data remain untouched while the composition root terminates
    /// the process.
    FailedClosed,
}

#[derive(Clone, Copy)]
enum PausedNativeContinuation {
    Cleanup { engine_verified: bool },
    Finalization,
}

pub(super) struct ProfileDeletionState {
    pub(super) phase: ProfileDeletionPhase,
    pub(super) operation_id: Option<String>,
    /// Session revision represented by the snapshot used for the latest
    /// authorization attempt. If journal proof arrives after this changes, the
    /// authorization barrier is still valid but the newer survivor state needs
    /// a fresh ordinary persistence pass after the logical tombstone lands.
    pub(super) authorization_revision: u128,
    pub(super) attempt_generation: u64,
    pub(super) retry_generation: u64,
    pub(super) retry_exponent: u8,
    pub(super) native_attempt: Option<RemovalAttempt>,
    initial_preparation: Option<Preparation>,
    recovery: Option<Box<dyn RecoverableRemoval>>,
    paused_native: Option<PausedNativeContinuation>,
    prepared: Option<Box<dyn PreparedRemoval>>,
    prepared_kind: Option<PreparedKind>,
    cleanup: zephium_ipc::ProfileRemovalCleanupV1,
    pub(super) native_progress: Option<(RemovalStage, bool)>,
}

impl ProfileDeletionState {
    pub(super) fn new(
        phase: ProfileDeletionPhase,
        operation_id: Option<String>,
        authorization_revision: u128,
    ) -> Self {
        let cleanup = if matches!(phase, ProfileDeletionPhase::CleanupReady { .. }) {
            zephium_ipc::ProfileRemovalCleanupV1::Unconfirmed
        } else {
            zephium_ipc::ProfileRemovalCleanupV1::NotStarted
        };
        Self {
            phase,
            operation_id,
            authorization_revision,
            attempt_generation: 0,
            retry_generation: 0,
            retry_exponent: 0,
            native_attempt: None,
            initial_preparation: None,
            recovery: None,
            paused_native: None,
            prepared: None,
            prepared_kind: None,
            cleanup,
            native_progress: None,
        }
    }
}

pub(super) type ProfileDeletionInbox =
    Arc<Mutex<std::collections::HashMap<ProfileId, (u64, ProfileDataErasureOutcome)>>>;

pub(super) struct ProfileDeletionCoordinator {
    pub(super) states: std::collections::HashMap<ProfileId, ProfileDeletionState>,
    pub(super) inbox: ProfileDeletionInbox,
    pub(super) batch_deadline: Option<std::time::Instant>,
    /// A failed invariant permanently closes deletion and bootstrap progress
    /// for this process while the composition root converges on shutdown.
    pub(super) failed_closed: bool,
    pub(super) native_participant: Option<Arc<dyn ProfileRemovalParticipant>>,
    pub(super) native_inventory: Option<InventoryStatus>,
}

impl Default for ProfileDeletionCoordinator {
    fn default() -> Self {
        let native_participant = crate::profile_removal::participant();
        let native_inventory = native_participant
            .as_ref()
            .map(|_| InventoryStatus::Unavailable);
        Self {
            states: std::collections::HashMap::new(),
            inbox: Arc::new(Mutex::new(std::collections::HashMap::new())),
            batch_deadline: None,
            failed_closed: false,
            native_participant,
            native_inventory,
        }
    }
}

impl Shell {
    pub(super) fn recover_native_profile_removals(&mut self, deadline: std::time::Instant) {
        let Some(participant) = self.profile_deletion.native_participant.clone() else {
            return;
        };
        let inventory = participant.recover_pending(deadline);
        self.apply_native_removal_inventory(&participant, inventory);
    }

    fn apply_native_removal_inventory(
        &mut self,
        participant: &Arc<dyn ProfileRemovalParticipant>,
        inventory: crate::profile_removal::RecoveryInventory,
    ) {
        let mut status = inventory.status;
        let mut seen = std::collections::HashSet::new();
        if inventory.records.len() > zephium_core::session::MAX_SESSION_PROFILES {
            status = InventoryStatus::Unavailable;
        }
        let finalized_pending = if inventory
            .records
            .iter()
            .any(|record| record.stage() == RecoveredRemovalStage::FinalizationPending)
        {
            match self.store.pending_profile_deletions() {
                ProfileDeletionLoad::Loaded(rows) => Some(
                    rows.into_iter()
                        .map(|row| row.profile)
                        .collect::<std::collections::HashSet<_>>(),
                ),
                ProfileDeletionLoad::Failed => None,
            }
        } else {
            None
        };
        for record in inventory
            .records
            .into_iter()
            .take(zephium_core::session::MAX_SESSION_PROFILES)
        {
            let profile = record.profile();
            if !seen.insert(profile) {
                status = InventoryStatus::Unavailable;
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.recovery = None;
                }
                continue;
            }
            if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                // Existing canonical journal supplies browser deletion authority;
                // this opaque record supplies only its own native recovery facts.
                if state.recovery.is_none() && state.prepared.is_none() {
                    state.recovery = Some(record);
                }
                continue;
            }
            if record.stage() == RecoveredRemovalStage::FinalizationPending {
                if self.profiles.get(profile).is_some()
                    || finalized_pending
                        .as_ref()
                        .is_none_or(|pending| pending.contains(&profile))
                {
                    status = InventoryStatus::Unavailable;
                    continue;
                }
                // The native participant already joined its own protected
                // Completed record to both canonical absent cohorts. Retain a
                // visible finalization-only obligation; no current action is
                // minted merely because this process restarted.
                let mut state = ProfileDeletionState::new(
                    ProfileDeletionPhase::AwaitingExplicitRetry,
                    None,
                    self.persistence.session_revision,
                );
                state.native_attempt = Some(RemovalAttempt::new(profile));
                state.recovery = Some(record);
                state.paused_native = Some(PausedNativeContinuation::Finalization);
                state.native_progress = Some((RemovalStage::RecoveryRequired, true));
                self.profile_deletion.states.insert(profile, state);
                continue;
            }
            if !self
                .profiles
                .get(profile)
                .is_some_and(|row| row.kind == ProfileKind::Named)
            {
                // Do not invent a visible profile or deletion authority for an
                // unjoinable native record. Seal the integration, not the browser.
                status = InventoryStatus::Unavailable;
                continue;
            }
            let mut state = ProfileDeletionState::new(
                ProfileDeletionPhase::AwaitingExplicitRetry,
                None,
                self.persistence.session_revision,
            );
            state.native_attempt = Some(RemovalAttempt::new(profile));
            state.recovery = Some(record);
            state.native_progress = Some((RemovalStage::RecoveryRequired, true));
            self.profile_deletion.states.insert(profile, state);
            self.user_content_status.retire_profile(profile);
            self.cancel_pending_blocker_mutation(
                profile,
                OperationReason::ProfileDeletionInProgress,
            );
            self.retire_blocker_statistics(profile);
            self.blocker.retire_profile(profile);
            self.finish_terminalized_blocker_native_operations();
        }
        if status == InventoryStatus::Unavailable {
            participant.seal_inventory();
        }
        self.profile_deletion.native_inventory = Some(status);
    }

    pub(super) fn recheck_profile_removal_inventory(
        &mut self,
    ) -> zephium_ipc::ProfileRemovalRetryV1 {
        let Some(participant) = self.profile_deletion.native_participant.clone() else {
            return zephium_ipc::ProfileRemovalRetryV1::NotPending;
        };
        let attempts = self
            .profile_deletion
            .states
            .values()
            .filter_map(|state| state.native_attempt.clone())
            .collect();
        let request = crate::profile_removal::InventoryRecheck::new(attempts);
        let inventory = participant.recheck_inventory(&request, self.profile_deletion_deadline());
        self.apply_native_removal_inventory(&participant, inventory);
        // Observation only: retain every operation, prepared token and phase.
        // A newly discovered preauth record remains AwaitingExplicitRetry.
        self.project_profiles();
        if self.profile_deletion.native_inventory == Some(InventoryStatus::Verified) {
            zephium_ipc::ProfileRemovalRetryV1::Scheduled
        } else {
            zephium_ipc::ProfileRemovalRetryV1::Blocked
        }
    }

    pub(super) fn profile_removal_recovery_view(
        &self,
    ) -> Option<zephium_ipc::ProfileRemovalRecoveryV1> {
        self.profile_deletion
            .native_inventory
            .map(|status| match status {
                InventoryStatus::Verified => zephium_ipc::ProfileRemovalRecoveryV1::Verified,
                InventoryStatus::Unavailable => zephium_ipc::ProfileRemovalRecoveryV1::Unavailable,
            })
    }

    pub(super) fn profile_removals_view(&self) -> Vec<zephium_ipc::ProfileRemovalViewV1> {
        use zephium_ipc::{
            ProfileRemovalNativeV1 as Native, ProfileRemovalPhaseV1 as Phase, ProfileRemovalViewV1,
        };
        let mut rows: Vec<_> = self
            .profile_deletion
            .states
            .iter()
            .map(|(profile, state)| ProfileRemovalViewV1 {
                profile: profile.to_string(),
                phase: match state.phase {
                    ProfileDeletionPhase::AwaitingExplicitRetry => Phase::AwaitingExplicitRetry,
                    ProfileDeletionPhase::PreparingNative => Phase::PreparingNative,
                    ProfileDeletionPhase::AwaitingAuthorization
                    | ProfileDeletionPhase::Authorizing { .. }
                    | ProfileDeletionPhase::ResolveAuthorizedJournal => Phase::Authorizing,
                    ProfileDeletionPhase::CleanupReady { .. } => Phase::Cleanup,
                    ProfileDeletionPhase::NativeReady
                    | ProfileDeletionPhase::NativeInFlight { .. } => Phase::ErasingBrowser,
                    ProfileDeletionPhase::FinalizeReady => Phase::Finalizing,
                    ProfileDeletionPhase::NativeFinalizationReady => Phase::AcknowledgingNative,
                    ProfileDeletionPhase::FailedClosed => Phase::FailedClosed,
                },
                native: match state.prepared_kind {
                    Some(PreparedKind::SignedOutSettled) => Native::SignedOutSettled,
                    Some(PreparedKind::NotConfigured) => Native::NotConfigured,
                    None => Native::Unconfirmed,
                },
                cleanup: state.cleanup,
                blocked: state.native_progress.is_some_and(|(_, blocked)| blocked)
                    || matches!(state.phase, ProfileDeletionPhase::FailedClosed),
            })
            .collect();
        rows.sort_by(|a, b| a.profile.cmp(&b.profile));
        rows
    }

    pub(super) fn begin_profile_deletion(
        &mut self,
        profile: ProfileId,
        operation_id: Option<String>,
    ) -> OperationDisposition {
        if self.profile_deletion.failed_closed {
            return operation_result(
                OperationOutcome::Rejected,
                OperationReason::StoreAdmissionRejected,
            );
        }
        if self.profile_deletion.native_inventory == Some(InventoryStatus::Unavailable) {
            return operation_result(
                OperationOutcome::Rejected,
                OperationReason::StoreAdmissionRejected,
            );
        }
        // One foreground deletion at a time keeps durable/native ownership
        // unambiguous. Crash-recovered journal rows may coexist, but a new
        // deletion waits until those obligations are resolved.
        if !self.profile_deletion.states.is_empty() {
            return operation_result(
                OperationOutcome::Rejected,
                OperationReason::ProfileDeletionInProgress,
            );
        }
        let Some(filtered) = self.filtered_session_for_profile_deletion(profile) else {
            return operation_result(
                OperationOutcome::Rejected,
                OperationReason::ProfileDeletionPolicyRejected,
            );
        };
        // Do not begin deletion while a native page can still execute, and do
        // not rely on a later best-effort close as proof. The actor's view bit
        // is the exact native lifecycle obligation: it remains set through
        // construction and discard until the corresponding native terminal
        // event.
        if self
            .items
            .view_ids()
            .into_iter()
            .any(|id| self.profile_of_item(id) == Some(profile))
        {
            return operation_result(
                OperationOutcome::Rejected,
                OperationReason::ProfileDeletionPolicyRejected,
            );
        }
        // Validate policy and canonical snapshot construction up front. The
        // snapshot is rebuilt at authorization so this preflight never becomes
        // authority.
        drop(filtered);
        let authorization_revision = self.persistence.session_revision;
        let native = self.profile_deletion.native_participant.is_some();
        let mut state = ProfileDeletionState::new(
            if native {
                ProfileDeletionPhase::PreparingNative
            } else {
                ProfileDeletionPhase::AwaitingAuthorization
            },
            operation_id,
            authorization_revision,
        );
        if let Some(participant) = self.profile_deletion.native_participant.clone() {
            let attempt = RemovalAttempt::new(profile);
            let result = participant.prepare(&attempt, self.profile_deletion_deadline());
            if matches!(result, Preparation::Busy) {
                return operation_result(
                    OperationOutcome::Rejected,
                    OperationReason::ProfileDeletionInProgress,
                );
            }
            // Native acceptance precedes quarantine. From here to insertion
            // there are no fallible checks/returns: retain this exact attempt
            // and move-only initial result for one consumption by the driver.
            state.native_attempt = Some(attempt);
            state.initial_preparation = Some(result);
        }
        self.profile_deletion.states.insert(profile, state);
        if self.user_content_status.retire_profile(profile) {
            self.project_runtime_status();
        }
        // From this point through durable tombstoning (or process restart),
        // the state row is a process-local quarantine and is never removed on
        // a retryable result.
        self.cancel_pending_blocker_mutation(profile, OperationReason::ProfileDeletionInProgress);
        self.retire_blocker_statistics(profile);
        self.blocker.retire_profile(profile);
        self.finish_terminalized_blocker_native_operations();
        operation_result(
            OperationOutcome::Deferred,
            OperationReason::StoreWorkPending,
        )
    }

    /// Builds the exact canonical post-removal state without mutating any
    /// aggregate. Only an inactive, non-default, non-private profile may pass.
    pub(super) fn filtered_session_for_profile_deletion(
        &self,
        profile: ProfileId,
    ) -> Option<session::SessionState> {
        let candidate = self.profiles.get(profile)?;
        if !self.bootstrapped
            || candidate.kind != ProfileKind::Named
            || self
                .windows
                .focused()
                .is_some_and(|window| window.profile == profile)
            || self
                .profiles
                .iter()
                .filter(|profile| profile.kind != ProfileKind::Incognito)
                .count()
                <= 1
        {
            return None;
        }

        let window = self.windows.focused();
        let mut filtered = session::snapshot_with_recently_closed(
            &self.profiles,
            &self.spaces,
            &self.items,
            window.map(|window| window.space),
            window.and_then(|window| window.active),
            window.and_then(|window| window.splits.as_ref()),
            &self.recently_closed,
        );
        if !filtered
            .profiles
            .iter()
            .any(|candidate| candidate.id == profile)
        {
            return None;
        }
        let removed_spaces: std::collections::HashSet<SpaceId> = filtered
            .spaces
            .iter()
            .filter(|space| space.profile == profile)
            .map(|space| space.id)
            .collect();
        filtered
            .profiles
            .retain(|candidate| candidate.id != profile);
        filtered
            .recently_closed
            .retain(|entry| entry.profile != profile);
        filtered.spaces.retain(|space| space.profile != profile);
        filtered.items.retain(|item| match item.placement {
            Placement::Favorites {
                profile: item_profile,
            } => item_profile != profile,
            Placement::Space { space, .. } => !removed_spaces.contains(&space),
        });

        (session::canonicalize(filtered.clone()) == filtered).then_some(filtered)
    }

    /// Applies only the logical half of an already-durable authorization.
    /// Native `Close` effects are deliberately suppressed: the engine's
    /// profile erasure owns closure and exact retirement of every view.
    pub(super) fn apply_profile_tombstone(&mut self, profile: ProfileId) {
        self.engine.forget_discarded_state(profile, None);
        self.recently_closed
            .retain(|entry| entry.profile != profile);
        self.extension_browser_surfaces.retire_profile(profile);
        self.extension_actions.retire_profile(profile);
        // Retire compiler admission and erase any callback result before the
        // native erasure boundary. A late list build can never reinstall
        // policy state for a journal-authorized profile.
        self.cancel_pending_blocker_mutation(profile, OperationReason::ProfileDeletionInProgress);
        self.retire_blocker_statistics(profile);
        self.blocker.retire_profile(profile);
        self.finish_terminalized_blocker_native_operations();
        let mut favicon_items: std::collections::HashSet<ItemId> = self
            .favicons
            .icon_attempts
            .iter()
            .filter_map(|(id, attempt)| (attempt.profile == profile).then_some(*id))
            .collect();
        favicon_items.extend(
            self.favicons
                .icon_load_completion_pending
                .iter()
                .filter_map(|(id, (item_profile, _))| (*item_profile == profile).then_some(*id)),
        );
        favicon_items.extend(
            self.favicons
                .store_reads
                .iter()
                .filter_map(|(id, pending)| (pending.profile == profile).then_some(*id)),
        );
        for id in favicon_items {
            self.cancel_favicon_attempt(id);
        }
        let discard_items: Vec<ItemId> = self
            .residency
            .discard_probes
            .keys()
            .copied()
            .filter(|id| self.profile_of_item(*id) == Some(profile))
            .collect();
        for id in discard_items {
            self.residency.discard_probes.remove(&id);
            if let Some(queue) = &self.self_queue {
                queue.cancel_discard_probe(id);
            }
        }

        let removed_spaces = self.spaces.remove_for_profile(profile);
        let _native_erasure_owns_close = self.items.remove_for_profile(&removed_spaces);
        self.profiles.remove(profile);
        self.forget_profile_roster(profile);

        self.residency
            .recent
            .retain(|id| self.items.tab(*id).is_some());
        self.residency
            .last_focus
            .retain(|id, _| self.items.tab(*id).is_some());
        self.last_visits
            .retain(|id, _| self.items.tab(*id).is_some());
        self.residency
            .dormant_sent
            .retain(|id| self.items.tab(*id).is_some());
        self.residency
            .discard_protected_until
            .retain(|id, _| self.items.tab(*id).is_some());
        self.crash
            .crashes
            .retain(|id, _| self.items.tab(*id).is_some());
        self.crash
            .presentations
            .retain(|id| self.items.tab(*id).is_some());
        self.favicons
            .icons_checked
            .retain(|(item_profile, _)| *item_profile != profile);
        self.favicons
            .icon_values
            .retain(|(item_profile, _), _| *item_profile != profile);
        self.favicons
            .icon_cache_order
            .retain(|(item_profile, _)| *item_profile != profile);
        if self
            .favicons
            .pending_batch
            .as_ref()
            .is_some_and(|pending| pending.profile == profile)
        {
            self.favicons.pending_batch = None;
        }
        if self
            .search
            .pending
            .as_ref()
            .is_some_and(|pending| pending.profile == profile)
        {
            self.search.pending = None;
        }

        // The authorization transaction already published the exact session.
        // An older debounce must never later overwrite that barrier.
        self.persistence.persist_first_dirty = None;
        self.persistence.url_checkpoint_dirty.clear();
        self.persistence.last_url_checkpoint = std::time::Instant::now();
        if let Some(queue) = &self.self_queue {
            queue.cancel_persist();
        }
        self.project_items();
    }

    pub(super) fn retry_profile_removal(
        &mut self,
        profile: ProfileId,
    ) -> zephium_ipc::ProfileRemovalRetryV1 {
        use zephium_ipc::ProfileRemovalRetryV1;
        let Some(state) = self.profile_deletion.states.get(&profile) else {
            return ProfileRemovalRetryV1::NotPending;
        };
        if self.profile_deletion.native_inventory == Some(InventoryStatus::Unavailable)
            || self.profile_deletion.failed_closed
            || matches!(state.phase, ProfileDeletionPhase::FailedClosed)
        {
            return ProfileRemovalRetryV1::Blocked;
        }
        // Keep the exact existing attempt, native deadlines and original
        // operation id. This never calls begin_profile_deletion or admits a
        // replacement logout. The native owner only reconciles its held work.
        if matches!(state.phase, ProfileDeletionPhase::AwaitingExplicitRetry) {
            self.retry_native_profile_removal(profile);
        } else {
            self.drive_profile_deletion(profile);
        }
        if self
            .profile_deletion
            .states
            .get(&profile)
            .is_some_and(|state| {
                state.native_progress.is_some_and(|(_, blocked)| blocked)
                    || matches!(state.phase, ProfileDeletionPhase::FailedClosed)
            })
        {
            ProfileRemovalRetryV1::Blocked
        } else {
            ProfileRemovalRetryV1::Scheduled
        }
    }

    pub(super) fn drive_profile_deletion(&mut self, profile: ProfileId) {
        if self.profile_deletion.native_inventory == Some(InventoryStatus::Unavailable) {
            if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                state.native_progress = Some((RemovalStage::RecoveryRequired, true));
            }
            self.project_profiles();
            return;
        }
        if self.profile_deletion.failed_closed {
            return;
        }
        enum Action {
            PrepareNative,
            CleanupNative(bool),
            Authorize,
            ReconcileAuthorization(bool),
            ResolveAuthorizedJournal,
            EraseNative,
            Finalize,
            AcknowledgeNative,
            None,
        }

        let action = {
            let Some(state) = self.profile_deletion.states.get_mut(&profile) else {
                return;
            };
            match state.phase {
                ProfileDeletionPhase::AwaitingExplicitRetry => Action::None,
                ProfileDeletionPhase::PreparingNative => Action::PrepareNative,
                ProfileDeletionPhase::CleanupReady { engine_verified } => {
                    Action::CleanupNative(engine_verified)
                }
                ProfileDeletionPhase::AwaitingAuthorization => Action::Authorize,
                ProfileDeletionPhase::Authorizing {
                    may_reauthorize, ..
                } => Action::ReconcileAuthorization(may_reauthorize),
                ProfileDeletionPhase::ResolveAuthorizedJournal => Action::ResolveAuthorizedJournal,
                ProfileDeletionPhase::NativeReady => Action::EraseNative,
                ProfileDeletionPhase::NativeInFlight { .. } => Action::None,
                ProfileDeletionPhase::FinalizeReady => Action::Finalize,
                ProfileDeletionPhase::NativeFinalizationReady => Action::AcknowledgeNative,
                ProfileDeletionPhase::FailedClosed => Action::None,
            }
        };
        if let Some(queue) = &self.self_queue {
            queue.cancel_profile_deletion(profile);
        }
        match action {
            Action::PrepareNative => self.prepare_native_profile_removal(profile),
            Action::CleanupNative(engine_verified) => {
                self.cleanup_native_profile_removal(profile, engine_verified)
            }
            Action::Authorize => {
                let deadline = self.profile_deletion_deadline();
                self.authorize_profile_deletion(profile, deadline);
                if !self.profile_deletion.failed_closed
                    && !self
                        .profile_deletion
                        .states
                        .get(&profile)
                        .is_some_and(|state| {
                            matches!(
                                state.phase,
                                ProfileDeletionPhase::AwaitingAuthorization
                                    | ProfileDeletionPhase::FailedClosed
                            )
                        })
                {
                    self.drive_profile_deletion(profile);
                }
            }
            Action::ReconcileAuthorization(may_reauthorize) => {
                self.reconcile_profile_authorization(profile, may_reauthorize)
            }
            Action::ResolveAuthorizedJournal => self.resolve_authorized_journal(profile),
            Action::EraseNative => {
                self.start_native_profile_erasure(profile);
                // A native adapter may have completed synchronously into the
                // bounded inbox.
                self.consume_profile_deletion_outcome(profile);
            }
            Action::Finalize => {
                let deadline = self.profile_deletion_deadline();
                self.finalize_profile_deletion(profile, deadline);
            }
            Action::AcknowledgeNative => self.acknowledge_native_finalization(profile, false),
            Action::None => {}
        }
    }

    /// Process-local quarantine established when deletion begins. Restart may
    /// clear a row that never reached Store authorization; this process may
    /// not.
    pub(super) fn profile_deletion_quarantines(&self, profile: ProfileId) -> bool {
        self.profile_deletion.states.contains_key(&profile)
    }

    pub(super) fn profile_deletion_quarantines_item(&self, id: ItemId) -> bool {
        self.profile_of_item(id)
            .is_some_and(|profile| self.profile_deletion_quarantines(profile))
    }

    fn profile_deletion_deadline(&self) -> std::time::Instant {
        self.profile_deletion.batch_deadline.unwrap_or_else(|| {
            let now = std::time::Instant::now();
            now.checked_add(PROFILE_DELETION_STORE_TIMEOUT)
                .unwrap_or(now)
        })
    }

    fn fail_profile_deletion_invariant(&mut self, profile: ProfileId) {
        crate::diagnostic!(
            "profile deletion: Store invariant failed; preserving durable profile data"
        );
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.phase = ProfileDeletionPhase::FailedClosed;
        }
        if let Some(queue) = &self.self_queue {
            queue.cancel_profile_deletion(profile);
        }
        self.profile_deletion.failed_closed = true;
        self.project_profiles();
        self.report_terminal_failure(ShellTerminalFailure::ProfileDeletionInvariant);
    }

    fn authorize_profile_deletion(&mut self, profile: ProfileId, deadline: std::time::Instant) {
        if !matches!(
            self.profile_deletion
                .states
                .get(&profile)
                .map(|state| &state.phase),
            Some(ProfileDeletionPhase::AwaitingAuthorization)
        ) {
            self.fail_profile_deletion_invariant(profile);
            return;
        }
        if self.profile_deletion.native_participant.is_some()
            && self
                .profile_deletion
                .states
                .get(&profile)
                .is_none_or(|state| state.prepared.is_none())
        {
            self.fail_profile_deletion_invariant(profile);
            return;
        }
        let Some(filtered) = self.filtered_session_for_profile_deletion(profile) else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.authorization_revision = self.persistence.session_revision;
        }
        if self.profile_deletion.native_participant.is_some() {
            let status = self
                .profile_deletion
                .states
                .get_mut(&profile)
                .and_then(|state| {
                    let attempt = state.native_attempt.as_ref()?;
                    let prepared = state.prepared.as_mut()?;
                    prepared
                        .attempt()
                        .same_attempt(attempt)
                        .then(|| prepared.authorization_status(attempt, deadline))
                });
            match status {
                Some(PreparedStatus::Current) => {}
                Some(PreparedStatus::Pending(stage)) => {
                    self.defer_native_profile_removal(profile, stage, false);
                    return;
                }
                Some(PreparedStatus::Interrupted(stage)) => {
                    self.pause_native_profile_removal(profile, stage);
                    return;
                }
                Some(PreparedStatus::Blocked(stage)) => {
                    self.defer_native_profile_removal(profile, stage, true);
                    return;
                }
                None => {
                    self.fail_profile_deletion_invariant(profile);
                    return;
                }
            }
        }
        // Once handed to Store, cancellation cannot prove no authorization.
        // Unknown acknowledgements always reconcile the canonical journal.
        let outcome = self
            .store
            .authorize_profile_deletion(profile, filtered, deadline);
        self.handle_profile_authorization_outcome(profile, outcome);
    }

    fn reconcile_profile_authorization(&mut self, profile: ProfileId, may_reauthorize: bool) {
        match self.store.pending_profile_deletions() {
            ProfileDeletionLoad::Loaded(pending) => {
                if let Some(deletion) = pending
                    .into_iter()
                    .find(|deletion| deletion.profile == profile)
                {
                    self.authorization_is_durable(profile, deletion);
                    self.drive_profile_deletion(profile);
                    return;
                }
                if !may_reauthorize {
                    self.schedule_profile_deletion_retry(profile);
                    return;
                }
                // The ordered absence proves the previous attempt did not
                // publish an authorization. Re-run policy now; the fresh
                // snapshot is built again at authorization.
                if self
                    .filtered_session_for_profile_deletion(profile)
                    .is_none()
                {
                    // Authorizing is reachable only after policy passed once;
                    // a profile that no longer qualifies means the Store and
                    // application authorities disagree.
                    self.fail_profile_deletion_invariant(profile);
                    return;
                }
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.authorization_revision = self.persistence.session_revision;
                    state.phase = ProfileDeletionPhase::AwaitingAuthorization;
                }
                self.drive_profile_deletion(profile);
            }
            ProfileDeletionLoad::Failed => self.schedule_profile_deletion_retry(profile),
        }
    }

    fn handle_profile_authorization_outcome(
        &mut self,
        profile: ProfileId,
        outcome: ProfileDeletionAuthorizeOutcome,
    ) {
        match outcome {
            ProfileDeletionAuthorizeOutcome::Authorized
            | ProfileDeletionAuthorizeOutcome::AlreadyAuthorized => {
                // The authorization result proves the session/journal
                // transaction, but it does not carry the durable native
                // namespace marker. Resolve the exact journal row before any
                // Engine erasure call; guessing `None` would lose authority.
                self.apply_profile_tombstone(profile);
                let Some(state) = self.profile_deletion.states.get_mut(&profile) else {
                    self.fail_profile_deletion_invariant(profile);
                    return;
                };
                state.phase = ProfileDeletionPhase::ResolveAuthorizedJournal;
                state.retry_exponent = 0;
            }
            ProfileDeletionAuthorizeOutcome::OutcomeUnknown => {
                // The ordered journal query is the only legal arbiter after a
                // deadline. Absence is not used to guess success; the next
                // bounded retry may re-run the idempotent authorization.
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.phase = ProfileDeletionPhase::Authorizing {
                        may_reauthorize: false,
                    };
                    state.retry_exponent = 0;
                }
            }
            ProfileDeletionAuthorizeOutcome::NotRegistered
            | ProfileDeletionAuthorizeOutcome::SessionConflict
            | ProfileDeletionAuthorizeOutcome::InvalidSession => {
                // Policy/canonical validation already passed. These outcomes
                // can only mean the Store and application authorities
                // disagree; reactivating the quarantined profile would be
                // unsafe.
                self.fail_profile_deletion_invariant(profile)
            }
            ProfileDeletionAuthorizeOutcome::NotAdmitted
            | ProfileDeletionAuthorizeOutcome::Failed => {
                // No terminal Store contradiction was proven. Retain the
                // quarantined state and retry without touching aggregate/native
                // data.
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.phase = ProfileDeletionPhase::AwaitingAuthorization;
                }
                self.schedule_profile_deletion_retry(profile);
            }
        }
    }

    fn resolve_authorized_journal(&mut self, profile: ProfileId) {
        match self.store.pending_profile_deletions() {
            ProfileDeletionLoad::Loaded(pending) => {
                if let Some(deletion) = pending
                    .into_iter()
                    .find(|deletion| deletion.profile == profile)
                {
                    self.authorization_is_durable(profile, deletion);
                    self.drive_profile_deletion(profile);
                } else {
                    // `AlreadyAuthorized` and a missing journal row conflict.
                    // Keep the logical tombstone and retry; never recreate the
                    // profile or infer that local deletion completed without
                    // an in-process native proof.
                    self.schedule_profile_deletion_retry(profile);
                }
            }
            ProfileDeletionLoad::Failed => self.schedule_profile_deletion_retry(profile),
        }
    }

    fn authorization_is_durable(&mut self, profile: ProfileId, deletion: PendingProfileDeletion) {
        if deletion.profile != profile {
            self.fail_profile_deletion_invariant(profile);
            return;
        }
        let survivor_state_changed = self
            .profile_deletion
            .states
            .get(&profile)
            .is_some_and(|state| state.authorization_revision != self.persistence.session_revision);
        self.apply_profile_tombstone(profile);
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.phase = if self.profile_deletion.native_participant.is_some() {
                ProfileDeletionPhase::CleanupReady {
                    engine_verified: deletion.native_erasure_verified,
                }
            } else if deletion.native_erasure_verified {
                ProfileDeletionPhase::FinalizeReady
            } else {
                ProfileDeletionPhase::NativeReady
            };
            state.retry_exponent = 0;
        }
        if survivor_state_changed {
            // The deletion authorization durably represents its own exact
            // snapshot, but newer survivor state was accepted while the reply
            // was uncertain. `apply_profile_tombstone` cancels the pre-barrier
            // debounce; establish a new post-barrier durability deadline
            // without pretending this scheduling pass is another mutation.
            self.schedule_current_session_persist();
        }
    }

    fn prepare_native_profile_removal(&mut self, profile: ProfileId) {
        let Some(participant) = self.profile_deletion.native_participant.clone() else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        let Some(attempt) = self
            .profile_deletion
            .states
            .get(&profile)
            .and_then(|state| state.native_attempt.clone())
        else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        if let Some(result) = self
            .profile_deletion
            .states
            .get_mut(&profile)
            .and_then(|state| state.initial_preparation.take())
        {
            self.accept_native_preparation(profile, &attempt, result);
            return;
        }
        let deadline = self.profile_deletion_deadline();
        if std::time::Instant::now() >= deadline {
            self.schedule_profile_deletion_retry(profile);
            return;
        }
        let result = if let Some(recovery) = self
            .profile_deletion
            .states
            .get_mut(&profile)
            .and_then(|state| state.recovery.as_mut())
        {
            recovery.reconcile(&attempt, deadline)
        } else {
            participant.prepare(&attempt, deadline)
        };
        self.accept_native_preparation(profile, &attempt, result);
    }

    fn accept_native_preparation(
        &mut self,
        profile: ProfileId,
        attempt: &RemovalAttempt,
        result: Preparation,
    ) {
        match result {
            Preparation::Busy => {
                self.pause_native_profile_removal(profile, RemovalStage::NativeIdle)
            }
            Preparation::Prepared(prepared) if prepared.attempt().same_attempt(attempt) => {
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.prepared_kind = Some(prepared.kind());
                    state.prepared = Some(prepared);
                    state.native_progress = None;
                    state.phase = ProfileDeletionPhase::AwaitingAuthorization;
                }
                self.project_profiles();
                self.drive_profile_deletion(profile);
            }
            Preparation::Prepared(_) => self.fail_profile_deletion_invariant(profile),
            Preparation::Interrupted(stage) => self.pause_native_profile_removal(profile, stage),
            Preparation::Pending(stage) => self.defer_native_profile_removal(profile, stage, false),
            Preparation::Blocked(stage) => self.defer_native_profile_removal(profile, stage, true),
        }
    }

    fn retry_native_profile_removal(&mut self, profile: ProfileId) {
        let deadline = self.profile_deletion_deadline();
        if std::time::Instant::now() >= deadline {
            return;
        }
        let Some(participant) = self.profile_deletion.native_participant.clone() else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        let Some(attempt) = self
            .profile_deletion
            .states
            .get(&profile)
            .and_then(|state| state.native_attempt.clone())
        else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        let retry = ExplicitRemovalRetry::new(attempt.clone());
        if self
            .profile_deletion
            .states
            .get(&profile)
            .is_some_and(|state| {
                matches!(
                    state.paused_native,
                    Some(PausedNativeContinuation::Finalization)
                )
            })
        {
            self.acknowledge_native_finalization(profile, true);
            return;
        }
        if let Some(PausedNativeContinuation::Cleanup { engine_verified }) = self
            .profile_deletion
            .states
            .get(&profile)
            .and_then(|state| state.paused_native)
        {
            let authorization = AuthorizedRemoval::new(attempt.clone());
            if let Some(prepared) = self
                .profile_deletion
                .states
                .get_mut(&profile)
                .and_then(|state| state.prepared.as_mut())
            {
                if !prepared.attempt().same_attempt(&attempt) {
                    self.fail_profile_deletion_invariant(profile);
                    return;
                }
                let result =
                    prepared.retry_cleanup_after_authorization(&authorization, &retry, deadline);
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.phase = ProfileDeletionPhase::CleanupReady { engine_verified };
                    state.paused_native = None;
                }
                self.accept_native_cleanup(profile, engine_verified, result);
                return;
            }
            let result = if let Some(recovery) = self
                .profile_deletion
                .states
                .get_mut(&profile)
                .and_then(|state| state.recovery.as_mut())
            {
                recovery.retry_authorized(&authorization, &retry, deadline)
            } else {
                participant.retry_authorized(&authorization, &retry, deadline)
            };
            if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                state.phase = ProfileDeletionPhase::CleanupReady { engine_verified };
                state.paused_native = None;
            }
            match result {
                Preparation::Busy => self.pause_authorized_native_profile_removal(
                    profile,
                    engine_verified,
                    RemovalStage::NativeIdle,
                ),
                Preparation::Prepared(prepared) if prepared.attempt().same_attempt(&attempt) => {
                    if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                        state.prepared_kind = Some(prepared.kind());
                        state.prepared = Some(prepared);
                    }
                    self.cleanup_native_profile_removal(profile, engine_verified);
                }
                Preparation::Prepared(_) => self.fail_profile_deletion_invariant(profile),
                Preparation::Interrupted(stage) => {
                    self.pause_authorized_native_profile_removal(profile, engine_verified, stage)
                }
                Preparation::Pending(stage) => {
                    self.defer_native_profile_removal(profile, stage, false)
                }
                Preparation::Blocked(stage) => {
                    self.defer_native_profile_removal(profile, stage, true)
                }
            }
            return;
        }
        if let Some(prepared) = self
            .profile_deletion
            .states
            .get_mut(&profile)
            .and_then(|state| state.prepared.as_mut())
        {
            if !prepared.attempt().same_attempt(&attempt) {
                self.fail_profile_deletion_invariant(profile);
                return;
            }
            let result = prepared.retry_before_authorization(&retry, deadline);
            match result {
                PreparedStatus::Current => {
                    if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                        state.phase = ProfileDeletionPhase::AwaitingAuthorization;
                        state.native_progress = None;
                    }
                    self.drive_profile_deletion(profile);
                }
                PreparedStatus::Pending(stage) => {
                    if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                        state.phase = ProfileDeletionPhase::AwaitingAuthorization;
                    }
                    self.defer_native_profile_removal(profile, stage, false);
                }
                PreparedStatus::Interrupted(stage) => {
                    self.pause_native_profile_removal(profile, stage)
                }
                PreparedStatus::Blocked(stage) => {
                    if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                        state.phase = ProfileDeletionPhase::AwaitingAuthorization;
                    }
                    self.defer_native_profile_removal(profile, stage, true);
                }
            }
            return;
        }
        let result = if let Some(recovery) = self
            .profile_deletion
            .states
            .get_mut(&profile)
            .and_then(|state| state.recovery.as_mut())
        {
            recovery.retry(&retry, deadline)
        } else {
            participant.retry(&retry, deadline)
        };
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.phase = ProfileDeletionPhase::PreparingNative;
        }
        self.accept_native_preparation(profile, &attempt, result);
    }

    fn pause_authorized_native_profile_removal(
        &mut self,
        profile: ProfileId,
        engine_verified: bool,
        stage: RemovalStage,
    ) {
        self.pause_native_profile_removal(profile, stage);
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.paused_native = Some(PausedNativeContinuation::Cleanup { engine_verified });
        }
    }

    fn pause_native_profile_removal(&mut self, profile: ProfileId, stage: RemovalStage) {
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.native_progress = Some((stage, true));
            state.phase = ProfileDeletionPhase::AwaitingExplicitRetry;
        }
        if let Some(queue) = &self.self_queue {
            queue.cancel_profile_deletion(profile);
        }
        self.project_profiles();
    }

    fn defer_native_profile_removal(
        &mut self,
        profile: ProfileId,
        stage: RemovalStage,
        blocked: bool,
    ) {
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.native_progress = Some((stage, blocked));
        }
        self.project_profiles();
        // The native owner retains its first deadline and uncertain send
        // evidence. This is reconciliation, not permission to resend logout.
        self.schedule_profile_deletion_retry(profile);
    }

    fn cleanup_native_profile_removal(&mut self, profile: ProfileId, engine_verified: bool) {
        let deadline = self.profile_deletion_deadline();
        if std::time::Instant::now() >= deadline {
            self.schedule_profile_deletion_retry(profile);
            return;
        }
        let Some(participant) = self.profile_deletion.native_participant.clone() else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        let attempt = {
            let Some(state) = self.profile_deletion.states.get_mut(&profile) else {
                return;
            };
            state
                .native_attempt
                .get_or_insert_with(|| RemovalAttempt::new(profile))
                .clone()
        };
        let authorization = AuthorizedRemoval::new(attempt.clone());
        if self
            .profile_deletion
            .states
            .get(&profile)
            .is_some_and(|state| state.prepared.is_none())
        {
            // Only a recovered, confirmed journal reaches this path without
            // the pre-authorization capability. Native protected provenance is
            // required; normal unregistered-profile grants are never used.
            let recovered = if let Some(record) = self
                .profile_deletion
                .states
                .get_mut(&profile)
                .and_then(|state| state.recovery.as_mut())
            {
                record.resume_authorized(&authorization, deadline)
            } else {
                participant.resume_authorized(&authorization, deadline)
            };
            match recovered {
                Preparation::Busy => {
                    self.pause_authorized_native_profile_removal(
                        profile,
                        engine_verified,
                        RemovalStage::NativeIdle,
                    );
                    return;
                }
                Preparation::Prepared(prepared) if prepared.attempt().same_attempt(&attempt) => {
                    if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                        state.prepared_kind = Some(prepared.kind());
                        state.prepared = Some(prepared);
                    }
                }
                Preparation::Prepared(_) => {
                    self.fail_profile_deletion_invariant(profile);
                    return;
                }
                Preparation::Interrupted(stage) => {
                    self.pause_authorized_native_profile_removal(profile, engine_verified, stage);
                    return;
                }
                Preparation::Pending(stage) => {
                    self.defer_native_profile_removal(profile, stage, false);
                    return;
                }
                Preparation::Blocked(stage) => {
                    self.defer_native_profile_removal(profile, stage, true);
                    return;
                }
            }
        }
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.cleanup = zephium_ipc::ProfileRemovalCleanupV1::Unconfirmed;
        }
        let result = self
            .profile_deletion
            .states
            .get_mut(&profile)
            .and_then(|state| state.prepared.as_mut())
            .map(|prepared| prepared.cleanup_after_authorization(&authorization, deadline));
        if let Some(result) = result {
            self.accept_native_cleanup(profile, engine_verified, result);
        } else {
            self.fail_profile_deletion_invariant(profile);
        }
    }

    fn accept_native_cleanup(
        &mut self,
        profile: ProfileId,
        engine_verified: bool,
        result: CleanupProgress,
    ) {
        match result {
            CleanupProgress::Verified => {
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.native_progress = None;
                    state.cleanup = zephium_ipc::ProfileRemovalCleanupV1::Verified;
                    state.phase = if engine_verified {
                        ProfileDeletionPhase::FinalizeReady
                    } else {
                        ProfileDeletionPhase::NativeReady
                    };
                }
                self.project_profiles();
                self.drive_profile_deletion(profile);
            }
            CleanupProgress::Interrupted(stage) => {
                self.pause_authorized_native_profile_removal(profile, engine_verified, stage)
            }
            CleanupProgress::Pending(stage) => {
                self.defer_native_profile_removal(profile, stage, false)
            }
            CleanupProgress::Blocked(stage) => {
                self.defer_native_profile_removal(profile, stage, true)
            }
        }
    }

    fn start_native_profile_erasure(&mut self, profile: ProfileId) {
        if !self
            .profile_deletion
            .states
            .get(&profile)
            .is_some_and(|state| matches!(state.phase, ProfileDeletionPhase::NativeReady))
        {
            self.fail_profile_deletion_invariant(profile);
            return;
        }
        let attempt = {
            let state = self
                .profile_deletion
                .states
                .get_mut(&profile)
                .expect("phase validation retained exact deletion state");
            state.attempt_generation = state.attempt_generation.wrapping_add(1);
            if state.attempt_generation == 0 {
                state.attempt_generation = 1;
            }
            let attempt = state.attempt_generation;
            state.phase = ProfileDeletionPhase::NativeInFlight { attempt };
            state.retry_exponent = 0;
            attempt
        };
        self.project_profiles();
        // Close the profile's notes before its folder is erased; the notes
        // thread handles this long before native erasure reports back.
        if let Some(notes) = &self.notes {
            notes.release(profile, Box::new(|| {}));
        }
        let inbox = self.profile_deletion.inbox.clone();
        let wake = self.self_queue.as_ref().map(|queue| CallbackHandle {
            queue: Arc::downgrade(&queue.inner),
        });
        self.engine.erase_profile_data(
            profile,
            Box::new(move |outcome| {
                let mut pending = inbox
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if pending.len() < zephium_core::session::MAX_SESSION_PROFILES
                    || pending.contains_key(&profile)
                {
                    pending.insert(profile, (attempt, outcome));
                }
                drop(pending);
                if let Some(wake) = wake {
                    let _ = wake.dispatch(Command::ProfileDeletionReady(profile));
                }
            }),
        );
    }

    pub(super) fn drain_profile_deletion_inbox(&mut self) {
        let profiles: Vec<ProfileId> = self
            .profile_deletion
            .inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .copied()
            .collect();
        for profile in profiles {
            self.consume_profile_deletion_outcome(profile);
        }
    }

    pub(super) fn consume_profile_deletion_outcome(&mut self, profile: ProfileId) {
        let pending = self
            .profile_deletion
            .inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&profile);
        let Some((attempt, outcome)) = pending else {
            return;
        };
        let exact = self
            .profile_deletion
            .states
            .get(&profile)
            .is_some_and(|state| {
                matches!(
                    state.phase,
                    ProfileDeletionPhase::NativeInFlight {
                        attempt: expected
                    } if expected == attempt
                )
            });
        if !exact {
            return;
        }
        match outcome {
            ProfileDataErasureOutcome::Verified => {
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.phase = ProfileDeletionPhase::FinalizeReady;
                    state.retry_exponent = 0;
                }
                self.drive_profile_deletion(profile);
            }
            ProfileDataErasureOutcome::Failed | ProfileDataErasureOutcome::TimedOut => {
                if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                    state.phase = ProfileDeletionPhase::NativeReady;
                }
                self.schedule_profile_deletion_retry(profile);
            }
        }
    }

    fn finalize_profile_deletion(&mut self, profile: ProfileId, deadline: std::time::Instant) {
        if !self
            .profile_deletion
            .states
            .get(&profile)
            .is_some_and(|state| matches!(state.phase, ProfileDeletionPhase::FinalizeReady))
        {
            self.fail_profile_deletion_invariant(profile);
            return;
        }
        if std::time::Instant::now() >= deadline {
            self.schedule_profile_deletion_retry(profile);
            return;
        }
        // Keep the existing durable deletion journal until the separately owned
        // native subscription record is erased. Failure is not a completed delete.
        #[cfg(feature = "work-runtime")]
        if !crate::work_models::erase_local_account_profiles(profile, deadline) {
            self.schedule_profile_deletion_retry(profile);
            return;
        }
        let outcome = self.store.finalize_profile_deletion(profile, deadline);
        match outcome {
            ProfileDeletionFinalizeOutcome::Completed => self.store_finalization_confirmed(profile),
            ProfileDeletionFinalizeOutcome::NotAuthorized => {
                // FinalizeReady is created only from a durable journal row
                // after native proof. Losing that authorization is a terminal
                // Store/application conflict.
                self.fail_profile_deletion_invariant(profile)
            }
            ProfileDeletionFinalizeOutcome::NotAdmitted
            | ProfileDeletionFinalizeOutcome::OutcomeUnknown
            | ProfileDeletionFinalizeOutcome::Failed => {
                if std::time::Instant::now() >= deadline {
                    self.schedule_profile_deletion_retry(profile);
                } else {
                    self.reconcile_profile_finalization(profile);
                }
            }
        }
    }

    fn reconcile_profile_finalization(&mut self, profile: ProfileId) {
        match self.store.pending_profile_deletions() {
            ProfileDeletionLoad::Loaded(pending) => {
                if pending.iter().any(|deletion| deletion.profile == profile) {
                    if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
                        state.phase = ProfileDeletionPhase::FinalizeReady;
                    }
                    self.schedule_profile_deletion_retry(profile);
                } else {
                    // Journal removal is the store's last ordered step, so an
                    // authoritative absence after native proof establishes
                    // both phases complete even if the RPC reply was lost.
                    self.store_finalization_confirmed(profile);
                }
            }
            ProfileDeletionLoad::Failed => {
                self.schedule_profile_deletion_retry(profile);
            }
        }
    }

    fn store_finalization_confirmed(&mut self, profile: ProfileId) {
        if self.profile_deletion.native_participant.is_none() {
            self.finish_profile_deletion_operation(
                profile,
                OperationOutcome::Applied,
                OperationReason::ProfileDeletionCompleted,
            );
            return;
        }
        let Some(state) = self.profile_deletion.states.get_mut(&profile) else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        state.phase = ProfileDeletionPhase::NativeFinalizationReady;
        state.paused_native = None;
        state.native_progress = None;
        self.project_profiles();
        self.acknowledge_native_finalization(profile, false);
    }

    fn acknowledge_native_finalization(&mut self, profile: ProfileId, explicit: bool) {
        let deadline = self.profile_deletion_deadline();
        if std::time::Instant::now() >= deadline {
            self.schedule_profile_deletion_retry(profile);
            return;
        }
        let Some(attempt) = self
            .profile_deletion
            .states
            .get(&profile)
            .and_then(|state| state.native_attempt.clone())
        else {
            self.fail_profile_deletion_invariant(profile);
            return;
        };
        let finalized = FinalizedRemoval::new(attempt.clone());
        let retry = ExplicitRemovalRetry::new(attempt.clone());
        let state = self
            .profile_deletion
            .states
            .get_mut(&profile)
            .expect("checked removal state");
        state.phase = ProfileDeletionPhase::NativeFinalizationReady;
        state.paused_native = None;
        let renew_held = explicit && state.prepared.is_some();
        if state.prepared.is_none() {
            let Some(recovery) = state.recovery.as_mut() else {
                self.fail_profile_deletion_invariant(profile);
                return;
            };
            let result = if explicit {
                recovery.retry_finalization(&retry, deadline)
            } else {
                recovery.resume_finalization(&finalized, deadline)
            };
            match result {
                Preparation::Busy => {
                    self.pause_native_finalization(profile, RemovalStage::NativeIdle);
                    return;
                }
                Preparation::Prepared(prepared) if prepared.attempt().same_attempt(&attempt) => {
                    state.prepared_kind = Some(prepared.kind());
                    state.prepared = Some(prepared);
                    state.cleanup = zephium_ipc::ProfileRemovalCleanupV1::Verified;
                }
                Preparation::Prepared(_) => {
                    self.fail_profile_deletion_invariant(profile);
                    return;
                }
                Preparation::Interrupted(stage) => {
                    self.pause_native_finalization(profile, stage);
                    return;
                }
                Preparation::Pending(stage) => {
                    self.defer_native_profile_removal(profile, stage, false);
                    return;
                }
                Preparation::Blocked(stage) => {
                    self.defer_native_profile_removal(profile, stage, true);
                    return;
                }
            }
        }
        let prepared = state.prepared.as_mut().expect("checked prepared custody");
        if !prepared.attempt().same_attempt(&attempt) {
            self.fail_profile_deletion_invariant(profile);
            return;
        }
        let result = if renew_held {
            prepared.retry_after_finalization(&finalized, &retry, deadline)
        } else {
            prepared.acknowledge_finalization(&finalized, deadline)
        };
        match result {
            CleanupProgress::Verified => self.finish_profile_deletion_operation(
                profile,
                OperationOutcome::Applied,
                OperationReason::ProfileDeletionCompleted,
            ),
            CleanupProgress::Interrupted(stage) => self.pause_native_finalization(profile, stage),
            CleanupProgress::Pending(stage) => {
                self.defer_native_profile_removal(profile, stage, false)
            }
            CleanupProgress::Blocked(stage) => {
                self.defer_native_profile_removal(profile, stage, true)
            }
        }
    }

    fn pause_native_finalization(&mut self, profile: ProfileId, stage: RemovalStage) {
        self.pause_native_profile_removal(profile, stage);
        if let Some(state) = self.profile_deletion.states.get_mut(&profile) {
            state.paused_native = Some(PausedNativeContinuation::Finalization);
        }
    }

    fn finish_profile_deletion_operation(
        &mut self,
        profile: ProfileId,
        outcome: OperationOutcome,
        reason: OperationReason,
    ) {
        if outcome == OperationOutcome::Applied
            && reason == OperationReason::ProfileDeletionCompleted
        {
            self.degraded_storage_profiles.remove(&profile);
        }
        let operation_id = self
            .profile_deletion
            .states
            .remove(&profile)
            .and_then(|state| state.operation_id);
        if let Some(queue) = &self.self_queue {
            queue.cancel_profile_deletion(profile);
        }
        self.profile_deletion
            .inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&profile);
        self.project_profiles();
        if let Some(operation_id) = operation_id {
            (self.emit)(Projection::OperationProcessed(OperationDisposition {
                operation_id,
                outcome,
                reason,
            }));
        }
    }

    pub(super) fn schedule_profile_deletion_retry(&mut self, profile: ProfileId) {
        let Some(state) = self.profile_deletion.states.get_mut(&profile) else {
            return;
        };
        if let ProfileDeletionPhase::Authorizing {
            may_reauthorize, ..
        } = &mut state.phase
        {
            *may_reauthorize = true;
        }
        state.retry_generation = state.retry_generation.wrapping_add(1);
        if state.retry_generation == 0 {
            state.retry_generation = 1;
        }
        let generation = state.retry_generation;
        let multiplier = 1_u32 << u32::from(state.retry_exponent.min(7));
        let delay = PROFILE_DELETION_RETRY_BASE
            .saturating_mul(multiplier)
            .min(PROFILE_DELETION_RETRY_MAX);
        state.retry_exponent = state.retry_exponent.saturating_add(1).min(7);
        if let Some(queue) = &self.self_queue {
            queue.schedule_profile_deletion(profile, generation, std::time::Instant::now() + delay);
        }
    }
}
