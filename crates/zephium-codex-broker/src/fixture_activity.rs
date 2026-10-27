//! Debug synthetic-cohort counters, never production telemetry.
use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) static DISCOVERY: AtomicU64 = AtomicU64::new(0);
pub(crate) static IMAGE: AtomicU64 = AtomicU64::new(0);
pub(crate) static SPAWN: AtomicU64 = AtomicU64::new(0);
pub(crate) static NETWORK: AtomicU64 = AtomicU64::new(0);
pub(crate) fn snapshot() -> [u64; 4] {
    [&DISCOVERY, &IMAGE, &SPAWN, &NETWORK].map(|counter| counter.load(Ordering::Acquire))
}
