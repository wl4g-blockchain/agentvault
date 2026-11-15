// SPDX-License-Identifier: GPL-3.0-or-later

//! Provider registration, lazy initialization and coordinated shutdown.

use anyhow::anyhow;

use anyhow::bail;

use anyhow::ensure;

use anyhow::Context;

use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::any::TypeId;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;

use super::provider::IProvider;
use crate::util::signal::ShutdownSignal;

type BuildFuture<T> = Pin<Box<dyn Future<Output = Result<Arc<T>>> + Send + 'static>>;
type Builder<T> = Box<dyn Fn() -> BuildFuture<T> + Send + Sync + 'static>;
type Key = (TypeId, String);
type Instances = HashMap<String, (i32, Arc<dyn IProvider>)>;

enum InstanceState<T: IProvider + ?Sized> {
    Empty,
    // Set before awaiting init. A cancelled or failed init requires cleanup.
    NeedsCleanup(Arc<T>),
    Ready(Arc<T>),
    // Some retains an unfinished close; None is a permanent closed state.
    Closing(Option<Arc<T>>),
    // An interface view delegates lifecycle ownership to its source wrapper.
    Alias(Arc<dyn IProvider>),
}

struct ProviderWrapper<T: IProvider + ?Sized> {
    build: Builder<T>,
    instance: AsyncMutex<InstanceState<T>>,
}

impl<T: IProvider + ?Sized> ProviderWrapper<T> {
    async fn get(&self) -> Result<Arc<T>> {
        let mut instance = self.instance.lock().await;
        match &*instance {
            InstanceState::Alias(_) => return (self.build)().await,
            InstanceState::Ready(value) => return Ok(Arc::clone(value)),
            InstanceState::Closing(_) => bail!("provider is closing"),
            InstanceState::NeedsCleanup(value) => {
                value
                    .close()
                    .await
                    .context("retrying incomplete initialization cleanup")?;
                *instance = InstanceState::Empty;
            }
            InstanceState::Empty => {}
        }

        let value = (self.build)().await?;
        *instance = InstanceState::NeedsCleanup(Arc::clone(&value));
        if let Err(error) = value.init().await {
            if let Err(cleanup) = value.close().await {
                return Err(anyhow!("provider init failed: {error:#}; cleanup failed: {cleanup:#}"));
            }
            *instance = InstanceState::Empty;
            return Err(error.context("provider initialization failed"));
        }
        *instance = InstanceState::Ready(Arc::clone(&value));
        Ok(value)
    }
}

#[async_trait]
impl<T: IProvider + ?Sized> IProvider for ProviderWrapper<T> {
    async fn close(&self) -> Result<()> {
        let mut instance = self.instance.lock().await;
        let value = match &*instance {
            InstanceState::Alias(source) => {
                source.close().await?;
                *instance = InstanceState::Closing(None);
                return Ok(());
            }
            InstanceState::Ready(value) | InstanceState::NeedsCleanup(value) | InstanceState::Closing(Some(value)) => {
                Some(Arc::clone(value))
            }
            InstanceState::Empty | InstanceState::Closing(None) => None,
        };
        *instance = InstanceState::Closing(value);
        if let InstanceState::Closing(Some(value)) = &*instance {
            value.close().await?; // Retain the instance on error or cancellation.
        }
        *instance = InstanceState::Closing(None);
        Ok(())
    }
}

#[derive(Default, Eq, PartialEq)]
enum ContextState {
    #[default]
    New,
    Running,
    Closing,
    Closed,
}

#[derive(Default)]
struct Registry {
    state: ContextState,
    registrations: HashMap<TypeId, Instances>,
}

/// A typed, process-local provider registry with lazy initialization.
///
/// The registry groups instances by domain interface `TypeId`, then by
/// `instance_id`. [`Self::init`] starts the context; a provider's
/// [`IProvider::init`] runs on its first get.
/// Instances of the same key share an `Arc`; different keys initialize
/// independently. Default shutdown phase 0 closes all entries concurrently.
/// Higher phases close first; register dependents with higher phases.
///
/// Application modules share [`Self::global`]. [`Self::new`] creates an
/// independent context, useful for tests and separately managed applications.
///
/// Call [`Self::close`] or [`Self::shutdown`] explicitly after stopping incoming
/// work and awaiting active tasks. Dropping the context cannot perform async
/// cleanup, and shutdown does not wait for externally held `Arc`s to drop.
#[derive(Default)]
pub struct ApplicationContext {
    registry: Mutex<Registry>,
    close_gate: AsyncMutex<()>,
}

impl ApplicationContext {
    /// Returns the same lazily created context to every module and thread.
    ///
    /// The application entry point registers providers, calls [`Self::init`],
    /// and eventually awaits [`Self::shutdown`]. Accessing the singleton does
    /// not initialize providers or restart a context that has been shut down.
    pub fn global() -> &'static Self {
        static CONTEXT: OnceLock<ApplicationContext> = OnceLock::new();
        CONTEXT.get_or_init(Self::new)
    }

    /// Creates an independent context with its own registrations and lifecycle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn key<T: IProvider + ?Sized>(instance: &str) -> Key {
        (TypeId::of::<T>(), instance.to_owned())
    }

    fn registry(&self) -> Result<MutexGuard<'_, Registry>> {
        self.registry
            .lock()
            .map_err(|_| anyhow!("application context registry mutex poisoned"))
    }

    /// Starts accepting get requests; repeated calls are harmless.
    /// Both pre-start registration and runtime registration are supported.
    ///
    /// # Errors
    /// Rejects a closing/closed registry or failure to install its process shutdown hook.
    pub async fn init(&self) -> Result<()> {
        std::future::ready(self.start(true)).await
    }

    /// Starts the registry while leaving signal handling and provider shutdown
    /// to a host that drains requests and worker tasks first.
    ///
    /// # Errors
    /// Rejects a context already closing or closed.
    pub async fn init_managed(&self) -> Result<()> {
        std::future::ready(self.start(false)).await
    }

    fn start(&self, install_signal_hook: bool) -> Result<()> {
        let mut registry = self.registry()?;
        match registry.state {
            ContextState::New => registry.state = ContextState::Running,
            ContextState::Running => {}
            ContextState::Closing | ContextState::Closed => bail!("application context is closing or closed"),
        }
        drop(registry);
        if install_signal_hook && std::ptr::eq(self, Self::global()) {
            Self::install_shutdown_hook()?;
        }
        Ok(())
    }

    fn install_shutdown_hook() -> Result<()> {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        let runtime = tokio::runtime::Handle::try_current().context("application context requires a Tokio runtime")?;
        if INSTALLED.set(()).is_ok() {
            runtime.spawn(async {
                ShutdownSignal::wait().await;
                if let Err(error) = ApplicationContext::global().close().await {
                    tracing::error!(%error, "application context signal shutdown failed");
                }
            });
        }
        Ok(())
    }

    /// Registers a factory without constructing or initializing its instance.
    ///
    /// Factories must clean up resources they create if they fail or are
    /// cancelled before returning an instance. The context owns lifecycle
    /// cleanup after the factory successfully returns its `Arc`.
    ///
    /// # Errors
    /// Rejects duplicate registrations or a registry that is closing or closed.
    pub fn register<T, Fut>(&self, instance_id: &str, create: impl Fn() -> Fut + Send + Sync + 'static) -> Result<()>
    where
        T: IProvider + ?Sized,
        Fut: Future<Output = Result<Arc<T>>> + Send + 'static,
    {
        self.register_with_shutdown_phase::<T, Fut>(instance_id, 0, create)
    }

    /// Atomically retains the first lazy constructor for this typed instance.
    /// Domain factories use the same instance ID for repeated acquisition; no
    /// losing constructor runs and no second registry or lock is needed.
    ///
    /// # Errors
    /// Reports registry lock failure or a registry that is closing or closed.
    pub fn register_if_absent<T, Fut>(
        &self,
        instance_id: &str,
        create: impl Fn() -> Fut + Send + Sync + 'static,
    ) -> Result<bool>
    where
        T: IProvider + ?Sized,
        Fut: Future<Output = Result<Arc<T>>> + Send + 'static,
    {
        let mut registry = self.registry()?;
        ensure!(
            matches!(registry.state, ContextState::New | ContextState::Running),
            "application context is closing or closed"
        );
        if registry
            .registrations
            .get(&TypeId::of::<T>())
            .is_some_and(|instances| instances.contains_key(instance_id))
        {
            return Ok(false);
        }
        Self::register_wrapper(
            &mut registry,
            instance_id,
            0,
            ProviderWrapper::<T> {
                build: Box::new(move || Box::pin(create())),
                instance: AsyncMutex::new(InstanceState::Empty),
            },
        )?;
        Ok(true)
    }

    /// Registers a factory with an explicit shutdown phase.
    ///
    /// A higher phase closes before a lower phase; entries inside a phase close
    /// concurrently. Register dependents with higher phases. The constructor
    /// follows the cancellation and cleanup contract of [`Self::register`].
    ///
    /// # Errors
    /// Rejects duplicate registrations or a registry that is closing or closed.
    pub fn register_with_shutdown_phase<T, Fut>(
        &self,
        instance_id: &str,
        shutdown_phase: i32,
        create: impl Fn() -> Fut + Send + Sync + 'static,
    ) -> Result<()>
    where
        T: IProvider + ?Sized,
        Fut: Future<Output = Result<Arc<T>>> + Send + 'static,
    {
        let mut registry = self.registry()?;
        Self::register_wrapper(
            &mut registry,
            instance_id,
            shutdown_phase,
            ProviderWrapper::<T> {
                build: Box::new(move || Box::pin(create())),
                instance: AsyncMutex::new(InstanceState::Empty),
            },
        )
    }

    /// Registers interface `T` with an existing `Source` instance.
    ///
    /// Application composition code calls this to expose another interface of
    /// the same object and share its lifecycle.
    ///
    /// The source must already be registered under `Source` with this instance
    /// ID. The projection runs lazily and must return an `Arc` view of that same
    /// object; a different allocation is rejected. No additional `init` runs.
    /// Closing either interface closes the shared object and invalidates every
    /// view. The alias inherits its source's shutdown phase and remains bound
    /// to that source even if its registration is later replaced.
    ///
    /// Use aliases for shared objects. Independent [`Self::register`] calls
    /// each own their factory and lifecycle.
    ///
    /// # Errors
    /// Rejects invalid or duplicate interface projections and unavailable source registrations.
    pub fn register_with<T, Source>(
        &self,
        instance_id: &str,
        project: impl Fn(Arc<Source>) -> Result<Arc<T>> + Send + Sync + 'static,
    ) -> Result<()>
    where
        T: IProvider + ?Sized,
        Source: IProvider + ?Sized,
    {
        let mut registry = self.registry()?;
        let source_key = Self::key::<Source>(instance_id);
        let (phase, source) = registry
            .registrations
            .get(&source_key.0)
            .and_then(|instances| instances.get(instance_id))
            .map(|(phase, source)| (*phase, Arc::clone(source)))
            .ok_or_else(|| anyhow!("alias source not registered: {source_key:?}"))?;
        let owner = Arc::clone(&source);
        let project = Arc::new(project);
        Self::register_wrapper(
            &mut registry,
            instance_id,
            phase,
            ProviderWrapper::<T> {
                build: Box::new(move || {
                    let (owner, project) = (Arc::clone(&owner), Arc::clone(&project));
                    Box::pin(async move {
                        let source = (owner.as_ref() as &dyn Any)
                            .downcast_ref::<ProviderWrapper<Source>>()
                            .ok_or_else(|| anyhow!("provider alias source type mismatch"))?
                            .get()
                            .await?;
                        let view = project(Arc::clone(&source))?;
                        ensure!(
                            Arc::as_ptr(&source).cast::<()>() == Arc::as_ptr(&view).cast::<()>(),
                            "provider alias must reference the source instance"
                        );
                        Ok(view)
                    })
                }),
                instance: AsyncMutex::new(InstanceState::Alias(source)),
            },
        )
    }

    fn register_wrapper<T: IProvider + ?Sized>(
        registry: &mut Registry,
        instance_id: &str,
        shutdown_phase: i32,
        wrapper: ProviderWrapper<T>,
    ) -> Result<()> {
        ensure!(!instance_id.trim().is_empty(), "instance_id must be nonempty");
        let key = Self::key::<T>(instance_id);
        ensure!(
            matches!(registry.state, ContextState::New | ContextState::Running),
            "application context is closing or closed"
        );
        let instances = registry.registrations.entry(key.0).or_default();
        ensure!(
            !instances.contains_key(instance_id),
            "duplicate provider registration: {key:?}"
        );
        instances.insert(key.1, (shutdown_phase, Arc::new(wrapper)));
        Ok(())
    }

    /// First get constructs and calls [`IProvider::init`]. Other concurrent
    /// getters of the same key await that work and reuse the same `Arc`.
    /// Callers can retain the returned `Arc` for repeated domain operations.
    ///
    /// # Errors
    /// Reports an unknown/unavailable registration or provider construction, initialization or cleanup failure.
    pub async fn get<T: IProvider + ?Sized>(&self, instance_id: &str) -> Result<Arc<T>> {
        let key = (TypeId::of::<T>(), instance_id);
        let entry = {
            let registry = self.registry()?;
            ensure!(
                registry.state == ContextState::Running,
                "application context is not running"
            );
            registry
                .registrations
                .get(&key.0)
                .and_then(|instances| instances.get(instance_id))
                .map(|(_, entry)| Arc::clone(entry))
                .ok_or_else(|| anyhow!("provider not registered: {key:?}"))?
        }; // No standard library lock is held across await.
        (entry.as_ref() as &dyn Any)
            .downcast_ref::<ProviderWrapper<T>>()
            .ok_or_else(|| anyhow!("provider registry type mismatch: {key:?}"))?
            .get()
            .await
            .with_context(|| format!("failed to get {key:?}"))
    }

    /// Closes a single instance and removes its factory on success.
    /// Once the instance lock is acquired, further gets are rejected. A failed
    /// or cancelled close retains the registration for retry. Missing keys are
    /// harmless; a removed key can be registered again while the context runs.
    ///
    /// # Errors
    /// Reports registry or provider shutdown failure; failed instances remain registered for retry.
    pub async fn close_for<T: IProvider + ?Sized>(&self, instance_id: &str) -> Result<()> {
        let key = Self::key::<T>(instance_id);
        let entry = {
            self.registry()?
                .registrations
                .get(&key.0)
                .and_then(|instances| instances.get(instance_id))
                .map(|(_, entry)| Arc::clone(entry))
        };
        if let Some(entry) = entry {
            entry
                .close()
                .await
                .with_context(|| format!("failed to close {key:?}"))?;
            self.remove_if_same(&key, &entry)?;
        }
        Ok(())
    }

    /// Closes every instance of one domain while keeping the application open.
    /// Failures retain their registrations for retry; every instance is attempted.
    ///
    /// # Errors
    /// Aggregates provider shutdown failures after attempting every registered instance of the domain.
    pub async fn close_all<T: IProvider + ?Sized>(&self) -> Result<()> {
        let ids: Vec<String> = self
            .registry()?
            .registrations
            .get(&TypeId::of::<T>())
            .map(|instances| instances.keys().cloned().collect())
            .unwrap_or_default();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(error) = self.close_for::<T>(&id).await {
                errors.push(format!("{id}: {error:#}"));
            }
        }
        ensure!(errors.is_empty(), "domain shutdown failed: {}", errors.join("; "));
        Ok(())
    }

    fn remove_if_same(&self, key: &Key, entry: &Arc<dyn IProvider>) -> Result<()> {
        let mut registry = self.registry()?;
        if let Some(instances) = registry.registrations.get_mut(&key.0) {
            if instances
                .get(&key.1)
                .is_some_and(|(_, existing)| Arc::ptr_eq(existing, entry))
            {
                instances.remove(&key.1);
            }
            if instances.is_empty() {
                registry.registrations.remove(&key.0);
            }
        }
        Ok(())
    }

    /// Gracefully shuts down in descending phases, concurrently per phase.
    ///
    /// Blocks new registration and get requests immediately. All phases are
    /// attempted, with failures aggregated and retained for retry. Concurrent
    /// calls serialize; successful closure is idempotent. If cancelled, call
    /// again to finish cleanup; the context remains closed to new work.
    ///
    /// # Errors
    /// Aggregates shutdown failures after attempting every provider in dependency order.
    pub async fn close(&self) -> Result<()> {
        {
            let mut registry = self.registry()?;
            if registry.state == ContextState::Closed {
                return Ok(());
            }
            registry.state = ContextState::Closing;
        }
        let _gate = self.close_gate.lock().await;

        let mut phases: BTreeMap<_, Vec<_>> = BTreeMap::new();
        {
            let registry = self.registry()?;
            for (domain, instances) in &registry.registrations {
                for (instance, (phase, entry)) in instances {
                    phases
                        .entry(*phase)
                        .or_default()
                        .push(((*domain, instance.clone()), Arc::clone(entry)));
                }
            }
        }

        let mut errors = Vec::new();
        for (_, entries) in phases.into_iter().rev() {
            let mut jobs = JoinSet::new();
            for (key, entry) in entries {
                jobs.spawn(async move {
                    let result = entry.close().await;
                    (key, entry, result)
                });
            }
            while let Some(joined) = jobs.join_next().await {
                match joined {
                    Ok((key, entry, Ok(()))) => {
                        if let Err(err) = self.remove_if_same(&key, &entry) {
                            errors.push(format!("{key:?}: {err:#}"));
                        }
                    }
                    Ok((key, _, Err(err))) => errors.push(format!("{key:?}: {err:#}")),
                    Err(err) => errors.push(format!("provider close task failed: {err}")),
                }
            }
        }

        {
            let mut registry = self.registry()?;
            if registry.registrations.is_empty() {
                registry.state = ContextState::Closed;
            }
        }
        if !errors.is_empty() {
            bail!("application shutdown failed: {}", errors.join("; "));
        }
        Ok(())
    }

    /// Alias for [`Self::close`].
    ///
    /// # Errors
    /// Returns the same registry or aggregated provider shutdown failures as `Self::close`.
    pub async fn shutdown(&self) -> Result<()> {
        self.close().await
    }
}
