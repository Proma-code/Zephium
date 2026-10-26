//! Shared shutdown ordering for the Windows account registries.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// Close admission under the service-creation lock, then return retained owners.
/// Callers close each owner only after this function releases both registry locks:
/// an owner's observer may synchronously re-enter the registry.
pub(crate) fn close<K, V>(
    services: &Mutex<HashMap<K, Arc<V>>>,
    foreground: &RwLock<Option<String>>,
    epoch: &AtomicU64,
    closed: &AtomicBool,
) -> Vec<Arc<V>> {
    let services = services.lock().unwrap_or_else(|error| error.into_inner());
    let mut foreground = foreground
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if !closed.swap(true, Ordering::AcqRel) {
        epoch.fetch_add(1, Ordering::AcqRel);
    }
    *foreground = None;
    // Repeated close still returns every retained owner. A concurrent caller
    // must not return before it has synchronously closed those owners itself.
    services.values().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[derive(Default)]
    struct Fixture {
        services: Mutex<HashMap<u8, Arc<u8>>>,
        foreground: RwLock<Option<String>>,
        epoch: AtomicU64,
        closed: AtomicBool,
    }
    impl Fixture {
        fn create(&self, id: u8) -> bool {
            let mut services = self.services.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return false;
            }
            services.insert(id, Arc::new(id));
            true
        }
        fn close(&self) -> Vec<Arc<u8>> {
            super::close(&self.services, &self.foreground, &self.epoch, &self.closed)
        }
    }
    #[test]
    fn subscription_registry_close_retires_scope_before_first_creation() {
        let registry = Fixture::default();
        *registry.foreground.write().unwrap() = Some("profile".into());
        assert!(registry.close().is_empty());
        assert!(!registry.create(1));
        assert!(registry.services.lock().unwrap().is_empty());
        assert!(registry.foreground.read().unwrap().is_none());
        assert_eq!(registry.epoch.load(Ordering::Acquire), 1);
        registry.close();
        assert_eq!(registry.epoch.load(Ordering::Acquire), 1);
    }
    #[test]
    fn subscription_registry_close_retains_creation_that_already_owns_the_lock() {
        let registry = Arc::new(Fixture::default());
        let mut services = registry.services.lock().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn({
            let registry = registry.clone();
            move || {
                started_tx.send(()).unwrap();
                done_tx.send(registry.close()).unwrap();
            }
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(done_rx.try_recv().is_err());
        services.insert(1, Arc::new(1));
        drop(services);
        let owners = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(owners.iter().map(|value| **value).collect::<Vec<_>>(), [1]);
        assert!(!registry.create(2));
        // Both registry locks are released before an owner's reentrant close.
        assert!(registry.services.try_lock().is_ok());
        assert!(registry.foreground.try_write().is_ok());
        assert_eq!(registry.close().len(), 1);
    }
}
