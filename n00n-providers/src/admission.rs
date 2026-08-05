//! Process and provider scoped admission for outbound model streams.

use async_lock::{Semaphore, SemaphoreGuardArc};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

pub const DEFAULT_MAX_CONCURRENT_STREAMS: usize = 8;
pub const DEFAULT_MAX_CONCURRENT_STREAMS_PER_PROVIDER: usize = 4;
const MAX_PROVIDER_KEYS: usize = 64;

pub struct ProviderAdmission {
    process: Arc<Semaphore>,
    provider_limit: usize,
    providers: Mutex<HashMap<String, Arc<Semaphore>>>,
    overflow: Arc<Semaphore>,
    active: AtomicUsize,
}

impl Default for ProviderAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderAdmission {
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(
            DEFAULT_MAX_CONCURRENT_STREAMS,
            DEFAULT_MAX_CONCURRENT_STREAMS_PER_PROVIDER,
        )
    }

    #[must_use]
    pub fn with_limits(process_limit: usize, provider_limit: usize) -> Self {
        Self {
            process: Arc::new(Semaphore::new(process_limit.max(1))),
            provider_limit: provider_limit.max(1),
            providers: Mutex::new(HashMap::new()),
            overflow: Arc::new(Semaphore::new(provider_limit.max(1))),
            active: AtomicUsize::new(0),
        }
    }

    #[must_use]
    pub fn global() -> &'static Self {
        static GLOBAL: LazyLock<ProviderAdmission> = LazyLock::new(ProviderAdmission::new);
        &GLOBAL
    }

    pub async fn acquire(&self, provider: &str) -> ProviderAdmissionGuard<'_> {
        let process = self.process.acquire_arc().await;
        let provider_sem = {
            let mut providers = self
                .providers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if providers.len() < MAX_PROVIDER_KEYS || providers.contains_key(provider) {
                Arc::clone(
                    providers
                        .entry(provider.to_owned())
                        .or_insert_with(|| Arc::new(Semaphore::new(self.provider_limit))),
                )
            } else {
                Arc::clone(&self.overflow)
            }
        };
        let provider = provider_sem.acquire_arc().await;
        self.active.fetch_add(1, Ordering::Relaxed);
        ProviderAdmissionGuard {
            process,
            provider,
            active: &self.active,
        }
    }

    #[must_use]
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }
}

pub struct ProviderAdmissionGuard<'a> {
    process: SemaphoreGuardArc,
    provider: SemaphoreGuardArc,
    active: &'a AtomicUsize,
}

impl Drop for ProviderAdmissionGuard<'_> {
    fn drop(&mut self) {
        let _ = (&self.process, &self.provider);
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_limit_covers_distinct_providers() {
        smol::block_on(async {
            let admission = Arc::new(ProviderAdmission::with_limits(1, 1));
            let first = admission.acquire("one").await;
            let (started_tx, started_rx) = flume::bounded(1);
            let admission_for_second = Arc::clone(&admission);
            let second = smol::spawn(async move {
                started_tx
                    .send_async(())
                    .await
                    .expect("test receiver remains available");
                let permit = admission_for_second.acquire("two").await;
                drop(permit);
            });
            started_rx
                .recv_async()
                .await
                .expect("waiter reached admission");
            drop(first);
            second.await;
            assert_eq!(admission.active(), 0);
        });
    }

    #[test]
    fn permit_releases_when_work_panics() {
        let admission = Arc::new(ProviderAdmission::with_limits(1, 1));
        let admission_for_work = Arc::clone(&admission);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            smol::block_on(async move {
                let _permit = admission_for_work.acquire("one").await;
                panic!("simulated stream panic");
            });
        }));
        assert!(result.is_err());
        assert_eq!(admission.active(), 0);
    }
}
