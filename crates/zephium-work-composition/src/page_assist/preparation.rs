//! Pre-say invocation lifetime, not a second admission queue or provider grant.
use super::*;
use std::future::Future;
use std::time::{Duration, Instant};

pub(super) const PREPARE_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) struct Preparing {
    pub profile: ProfileId,
    pub identity: Arc<()>,
    pub cancel: Arc<Cancel>,
}

pub(super) struct Preparation {
    service: PageAssist,
    space: String,
    conversation: String,
    profile: ProfileId,
    pub current_epoch: Arc<()>,
    pub revision: Arc<()>,
    identity: Arc<()>,
    pub cancel: Arc<Cancel>,
    pub deadline: Instant,
    committed: bool,
    native_deadline: bool,
}

fn native(fault: PageAssistNativeFaultV1) -> PageAssistFaultV1 {
    PageAssistFaultV1::NativeConversation { fault }
}

impl Preparation {
    /// Captures before any wake/entry await. The map is bounded by live
    /// conversations and holds no draft, native thread, or extra queue permit.
    pub fn begin(
        service: &PageAssist,
        space: &str,
        conversation: &str,
    ) -> Result<Self, PageAssistFaultV1> {
        let mut state = service.state();
        let live = state
            .current
            .get(space)
            .filter(|id| id.as_str() == conversation)
            .and_then(|id| state.live.get(id))
            .ok_or(PageAssistFaultV1::Invalid)?;
        let profile = live.profile;
        let revision = live.revision.clone();
        let current_epoch = state
            .current_epochs
            .get(space)
            .cloned()
            .ok_or(PageAssistFaultV1::Invalid)?;
        if !service.ports().settings.ai_enabled(&profile) {
            return Err(PageAssistFaultV1::AiOff);
        }
        if service.ports().tabs.profile(space) != Some(profile) {
            return Err(native(PageAssistNativeFaultV1::ScopeChanged));
        }
        if state.preparing.contains_key(conversation)
            || state.running.contains_key(conversation)
            || state
                .native_owners
                .values()
                .any(|owner| owner.blocks(profile))
        {
            return Err(PageAssistFaultV1::Busy);
        }
        let identity = Arc::new(());
        let cancel = Arc::new(Cancel::default());
        state.preparing.insert(
            conversation.to_owned(),
            Preparing {
                profile,
                identity: identity.clone(),
                cancel: cancel.clone(),
            },
        );
        Ok(Self {
            service: service.clone(),
            space: space.to_owned(),
            conversation: conversation.to_owned(),
            profile,
            current_epoch,
            revision,
            identity,
            cancel,
            deadline: Instant::now() + PREPARE_TIMEOUT,
            committed: false,
            native_deadline: false,
        })
    }

    pub fn check(&self, state: &State) -> Result<(), PageAssistFaultV1> {
        if let Some(reason) = self.cancel.reason() {
            return Err(native(match reason {
                CancelReason::Stopped => PageAssistNativeFaultV1::Cancelled,
                CancelReason::Gone => PageAssistNativeFaultV1::ScopeChanged,
            }));
        }
        if self.native_deadline && Instant::now() >= self.deadline {
            return Err(native(PageAssistNativeFaultV1::Timeout));
        }
        if !self.service.ports().settings.ai_enabled(&self.profile) {
            return Err(PageAssistFaultV1::AiOff);
        }
        if self.service.ports().tabs.profile(&self.space) != Some(self.profile)
            || state.current.get(&self.space) != Some(&self.conversation)
            || state
                .current_epochs
                .get(&self.space)
                .is_none_or(|epoch| !Arc::ptr_eq(epoch, &self.current_epoch))
            || state.live.get(&self.conversation).is_none_or(|live| {
                live.profile != self.profile || !Arc::ptr_eq(&live.revision, &self.revision)
            })
            || state.preparing.get(&self.conversation).is_none_or(|entry| {
                entry.profile != self.profile
                    || !Arc::ptr_eq(&entry.identity, &self.identity)
                    || !Arc::ptr_eq(&entry.cancel, &self.cancel)
            })
        {
            return Err(native(PageAssistNativeFaultV1::ScopeChanged));
        }
        if state.running.contains_key(&self.conversation) {
            return Err(PageAssistFaultV1::Busy);
        }
        Ok(())
    }

    /// Enables only the native demand ceiling captured at begin, never a new
    /// deadline. Legacy Model entry/attachments retain their existing timing.
    pub fn native_demand(&mut self) {
        self.native_deadline = true;
    }

    pub async fn wait<T>(&self, future: impl Future<Output = T>) -> Result<T, PageAssistFaultV1> {
        self.wait_inner(self.native_deadline.then_some(self.deadline), future)
            .await
    }

    pub async fn wait_until<T>(
        &self,
        deadline: Instant,
        future: impl Future<Output = T>,
    ) -> Result<T, PageAssistFaultV1> {
        self.wait_inner(
            Some(if self.native_deadline {
                deadline.min(self.deadline)
            } else {
                deadline
            }),
            future,
        )
        .await
    }

    async fn wait_inner<T>(
        &self,
        deadline: Option<Instant>,
        future: impl Future<Output = T>,
    ) -> Result<T, PageAssistFaultV1> {
        self.check(&self.service.state())?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(native(PageAssistNativeFaultV1::Timeout));
        }
        let timeout = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                None => std::future::pending::<()>().await,
            }
        };
        let result = tokio::select! {
            biased;
            reason = self.cancel.cancelled() => return Err(native(match reason {
                CancelReason::Stopped => PageAssistNativeFaultV1::Cancelled,
                CancelReason::Gone => PageAssistNativeFaultV1::ScopeChanged,
            })),
            _ = timeout => return Err(native(PageAssistNativeFaultV1::Timeout)),
            result = future => result,
        };
        self.check(&self.service.state())?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(native(PageAssistNativeFaultV1::Timeout));
        }
        Ok(result)
    }

    /// Called under the SAME lock that inserts Running, after validation and
    /// immediately before visible history changes. Stop sees one owner or the
    /// other; both share the unchanged cancellation bit.
    pub fn commit(&mut self, state: &mut State) -> Result<(), PageAssistFaultV1> {
        self.check(state)?;
        state.preparing.remove(&self.conversation);
        self.committed = true;
        Ok(())
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.cancel.cancel(CancelReason::Gone);
        let mut state = self.service.state();
        if state
            .preparing
            .get(&self.conversation)
            .is_some_and(|entry| Arc::ptr_eq(&entry.identity, &self.identity))
        {
            state.preparing.remove(&self.conversation);
        }
    }
}

pub(super) fn backend_fault(error: PageBackendError) -> PageAssistFaultV1 {
    match error {
        PageBackendError::Busy => PageAssistFaultV1::Busy,
        PageBackendError::Model(error) => fault::model_fault(error, false),
        PageBackendError::NativeConversation(fault) => native(fault),
    }
}
