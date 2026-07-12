use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

use tokio::sync::OnceCell;

use crate::EnvironmentProviderAdapter;
use crate::EnvironmentProviderAdapterError;
use crate::EnvironmentProviderAdapterFactory;
use crate::EnvironmentProviderService;
use crate::EnvironmentProviderServiceError;

type AdapterCell = OnceCell<Arc<dyn EnvironmentProviderAdapter>>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveEnvironmentProviderAdapterError {
    #[error(transparent)]
    Configuration(#[from] EnvironmentProviderServiceError),
    #[error(transparent)]
    Adapter(#[from] EnvironmentProviderAdapterError),
}

/// Lazily constructs and retains one adapter for each resolved provider definition.
#[derive(Clone)]
pub(crate) struct EnvironmentProviderAdapterPool {
    configuration: EnvironmentProviderService,
    factory: Arc<dyn EnvironmentProviderAdapterFactory>,
    adapters: Arc<Mutex<HashMap<String, Arc<AdapterCell>>>>,
}

impl std::fmt::Debug for EnvironmentProviderAdapterPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentProviderAdapterPool")
            .field("configuration", &self.configuration)
            .finish_non_exhaustive()
    }
}

impl EnvironmentProviderAdapterPool {
    pub(crate) fn new(
        configuration: EnvironmentProviderService,
        factory: Arc<dyn EnvironmentProviderAdapterFactory>,
    ) -> Self {
        Self {
            configuration,
            factory,
            adapters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn adapter(
        &self,
        provider_id: &str,
    ) -> Result<Arc<dyn EnvironmentProviderAdapter>, ResolveEnvironmentProviderAdapterError> {
        let cell = {
            let mut adapters = self.adapters.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(
                adapters
                    .entry(provider_id.to_string())
                    .or_insert_with(|| Arc::new(OnceCell::new())),
            )
        };
        let adapter = cell
            .get_or_try_init(|| async {
                let definition = self
                    .configuration
                    .resolve_provider(provider_id.to_string())
                    .await?;
                self.factory
                    .create_adapter(definition)
                    .await
                    .map_err(ResolveEnvironmentProviderAdapterError::from)
            })
            .await?;
        Ok(Arc::clone(adapter))
    }

    pub(crate) fn invalidate(&self, provider_id: &str) {
        self.adapters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(provider_id);
    }
}
