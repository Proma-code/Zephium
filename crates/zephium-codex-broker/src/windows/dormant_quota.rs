//! Move-only close-drain of already-expired observational RPCs. It cannot send,
//! mint account/quota freshness, ignore notifications, or cover another Session.
use super::write_receipt::SessionStamp;
use crate::{conversation::Scope, wire};
use std::collections::BTreeMap;
const MAX_QUOTA_READS: usize = 512;
pub(crate) struct DormantQuotaDrain {
    stamp: SessionStamp,
    scope: Scope,
    // Retain original native epoch with each exact globally allocated RPC ID.
    pending: BTreeMap<u64, u64>,
}
impl DormantQuotaDrain {
    pub(super) fn new(stamp: SessionStamp, scope: Scope, expired: Vec<(u64, u64)>) -> Option<Self> {
        if expired.is_empty() || expired.len() > MAX_QUOTA_READS {
            return None;
        }
        let mut pending = BTreeMap::new();
        for (id, epoch) in expired {
            if id == 0 || id > i64::MAX as u64 || epoch == 0 || pending.insert(id, epoch).is_some()
            {
                return None;
            }
        }
        Some(Self {
            stamp,
            scope,
            pending,
        })
    }
    pub(super) fn joins(&self, stamp: &SessionStamp, scope: &Scope) -> bool {
        self.stamp.same(stamp) && self.scope == *scope
    }
    pub(super) fn observe(&mut self, bytes: &[u8]) -> Result<(), ()> {
        // Exact envelope validation only. Values are intentionally never fed
        // back to QuotaOwner or classified as a successful current reading.
        match wire::decode(bytes).map_err(|_| ())? {
            wire::Incoming::Reply { id, .. } if self.pending.remove(&id).is_some() => Ok(()),
            _ => Err(()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope::new("owned".into(), [1; 16], 1).unwrap()
    }
    #[test]
    fn exact_expired_quota_result_or_error_is_consumed_once_without_projection() {
        let stamp = SessionStamp::new();
        let mut drain =
            DormantQuotaDrain::new(stamp.clone(), scope(), vec![(8, 2), (9, 1)]).unwrap();
        assert!(drain.joins(&stamp, &scope()));
        assert!(drain
            .observe(br#"{"id":8,"result":{"rateLimits":{"primary":{"usedPercent":9}}}}"#)
            .is_ok());
        assert!(drain
            .observe(br#"{"id":9,"error":{"code":-1,"message":"owned refusal"}}"#)
            .is_ok());
        assert!(drain.pending.is_empty());
        assert!(drain.observe(br#"{"id":8,"result":{}}"#).is_err());
    }
    #[test]
    fn foreign_session_scope_id_duplicate_and_any_account_or_tool_frame_refuse() {
        let stamp = SessionStamp::new();
        let foreign = SessionStamp::new();
        let mut drain = DormantQuotaDrain::new(stamp.clone(), scope(), vec![(8, 2)]).unwrap();
        assert!(!drain.joins(&foreign, &scope()));
        assert!(!drain.joins(&stamp, &Scope::new("owned".into(), [1; 16], 2).unwrap()));
        for bytes in [
            br#"{"id":7,"result":{}}"#.as_slice(),
            br#"{"id":8,"id":8,"result":{}}"#,
            br#"{"id":8,"result":null}"#,
            br#"{"method":"account/updated","params":{"authMode":null}}"#,
            br#"{"id":8,"method":"item/tool/call","params":{}}"#,
        ] {
            assert!(drain.observe(bytes).is_err());
        }
        assert_eq!(drain.pending.get(&8), Some(&2));
    }
    #[test]
    fn absent_reply_at_eof_is_still_unknown_not_read_success_or_retry_authority() {
        let drain = DormantQuotaDrain::new(SessionStamp::new(), scope(), vec![(8, 1)]).unwrap();
        assert_eq!(drain.pending.len(), 1);
        // No method exposes results, new IDs, dispatch or an account receipt.
        drop(drain);
        assert!(DormantQuotaDrain::new(SessionStamp::new(), scope(), vec![]).is_none());
        assert!(
            DormantQuotaDrain::new(SessionStamp::new(), scope(), vec![(1, 1), (1, 2)]).is_none()
        );
        assert!(DormantQuotaDrain::new(SessionStamp::new(), scope(), vec![(0, 1)]).is_none());
        assert!(DormantQuotaDrain::new(SessionStamp::new(), scope(), vec![(1, 0)]).is_none());
    }
}
