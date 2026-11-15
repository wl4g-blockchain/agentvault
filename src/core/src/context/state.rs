//! Application-scoped providers; the host drains work before shutdown.
use crate::{
    cache::{CacheComponent, CacheFactory, ICache, ICacheProvider},
    config::AppConfig,
    storage::{IRepository, IStorageProvider, StorageComponent, StorageFactory},
};
use anyhow::{Context as _, Result};
use common_context::ApplicationContext;
use std::sync::Arc;

/// Production shares one process registry; tests can deliberately isolate it.
pub enum ApplicationScope {
    Shared,
    Isolated(Arc<ApplicationContext>),
}
impl ApplicationScope {
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::Shared)
    }
    #[must_use]
    pub fn isolated() -> Arc<Self> {
        Arc::new(Self::Isolated(Arc::new(ApplicationContext::new())))
    }
    #[must_use]
    pub fn context(&self) -> &ApplicationContext {
        match self {
            Self::Shared => ApplicationContext::global(),
            Self::Isolated(context) => context,
        }
    }
}

/// Initialized cache and SQL handles, managed once by their typed SPI owners.
pub struct RuntimeProviders {
    application: Arc<ApplicationScope>,
    pub cache: Arc<dyn ICache>,
    pub storage: Arc<dyn IRepository>,
}
impl RuntimeProviders {
    /// Bootstraps adapters in their established order, then registers shared SPI owners.
    ///
    /// # Errors
    /// Returns initialization failures after closing every acquired provider.
    pub async fn open(config: &AppConfig, application: Arc<ApplicationScope>) -> Result<Self> {
        // SQLx migration futures stay on the existing composition task. Provider
        // registration then shares the initialized adapters without rerunning IO.
        let cache = CacheFactory::new()
            .create(&config.cache)
            .await
            .context("initialize non-authoritative cache")?;
        let storage = match StorageFactory::new().create(&config.storage).await {
            Ok(storage) => storage,
            Err(error) => {
                let _ = cache.close().await;
                return Err(error).context("initialize authoritative storage");
            }
        };
        let result = Self::from_parts(application, Arc::clone(&cache), Arc::clone(&storage)).await;
        if result.is_err() {
            let _ = cache.close().await;
            let _ = storage.close().await;
        }
        result
    }

    /// Attaches explicitly supplied adapters, keeping shared lifecycle ownership.
    ///
    /// # Errors
    /// Returns registration, startup or acquisition failures after cleanup.
    pub async fn from_parts(
        application: Arc<ApplicationScope>,
        cache: Arc<dyn ICache>,
        storage: Arc<dyn IRepository>,
    ) -> Result<Self> {
        application
            .context()
            .register_with_shutdown_phase::<dyn ICacheProvider, _>("cache-default", 10, move || {
                let cache = Arc::clone(&cache);
                async move { Ok(Arc::new(CacheComponent::new(cache)) as Arc<dyn ICacheProvider>) }
            })?;
        application
            .context()
            .register_with_shutdown_phase::<dyn IStorageProvider, _>("storage-default", 0, move || {
                let storage = Arc::clone(&storage);
                async move { Ok(Arc::new(StorageComponent::new(storage)) as Arc<dyn IStorageProvider>) }
            })?;
        Self::acquire(application).await
    }

    async fn acquire(application: Arc<ApplicationScope>) -> Result<Self> {
        // No automatic signal hook: HTTP and worker shutdown must finish first.
        application.context().init_managed().await?;
        let acquired: Result<_> = async {
            let cache = application
                .context()
                .get::<dyn ICacheProvider>("cache-default")
                .await?
                .cache();
            let storage = application
                .context()
                .get::<dyn IStorageProvider>("storage-default")
                .await?
                .repository();
            Ok((cache, storage))
        }
        .await;
        match acquired {
            Ok((cache, storage)) => Ok(Self {
                application,
                cache,
                storage,
            }),
            Err(error) => {
                let _ = application.context().shutdown().await;
                Err(error)
            }
        }
    }

    /// Stops every provider in cache-before-SQL order after the host drains work.
    ///
    /// # Errors
    /// Reports shutdown failures while still attempting all providers.
    pub async fn close(&self) -> Result<()> {
        self.application.context().shutdown().await
    }
}

#[cfg(test)]
#[path = "../../tests/context/state_test.rs"]
mod tests;
