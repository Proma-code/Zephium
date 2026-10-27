//! Reject retained references to retired subscription sources before any client
//! call. Original API-key and Cloud clients pass through unchanged.
use super::WorkLeadModels;
use std::sync::Arc;
use zephium_core::ids::ProfileId;
use zephium_core::work::model::*;

struct RetiredClient(WorkModelError);
impl WorkModelClient for RetiredClient {
    fn call<'a>(
        &'a self,
        _: WorkModelRequest,
        _: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> WorkModelFuture<'a> {
        Box::pin(async move { Err(self.0) })
    }
}

/// A defensive admission boundary even for clients supplied outside the picker.
/// Retired clients are dropped, never invoked, retried, or replaced with BYOK.
pub fn guard_run_sources(_profile: ProfileId, mut models: WorkLeadModels) -> WorkLeadModels {
    for model in [&mut models.lead, &mut models.page, &mut models.light] {
        if let Some(error) = crate::work_models::retired_source_error(&model.entry) {
            model.client = Arc::new(RetiredClient(error));
        }
    }
    models
}

#[cfg(test)]
mod tests;
