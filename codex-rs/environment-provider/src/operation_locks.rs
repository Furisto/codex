use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::Weak;

use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::OwnedMutexGuard;

/// Serializes mutating environment operations independently for each provider definition.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderOperationLocks {
    locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

impl ProviderOperationLocks {
    pub(crate) async fn lock(&self, provider_id: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(provider_id).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(AsyncMutex::new(()));
                    locks.insert(provider_id.to_string(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}
