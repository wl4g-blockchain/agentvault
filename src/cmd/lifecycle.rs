use std::time::Duration;

use anyhow::{anyhow, Context as _};
use async_trait::async_trait;
use tokio::{sync::watch, task::JoinSet};

#[async_trait]
pub trait RuntimeComponent: Send {
    fn name(&self) -> &'static str;

    /// Runs until shutdown or a component failure.
    async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()>;
}

/// Long-lived provider registry closed only after all request handlers and
/// background components have completed graceful shutdown.
#[async_trait]
pub trait RuntimeResource: Send {
    fn name(&self) -> &'static str;

    async fn close(self: Box<Self>) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub struct Shutdown {
    receiver: watch::Receiver<bool>,
}

impl Shutdown {
    pub async fn cancelled(&mut self) {
        while !*self.receiver.borrow() {
            if self.receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

pub struct ServerSupervisor {
    components: Vec<Box<dyn RuntimeComponent>>,
    shutdown_timeout: Duration,
}

impl ServerSupervisor {
    #[must_use]
    pub fn new(shutdown_timeout: Duration) -> Self {
        Self {
            components: Vec::new(),
            shutdown_timeout,
        }
    }

    pub fn add<C>(&mut self, component: C)
    where
        C: RuntimeComponent + 'static,
    {
        self.components.push(Box::new(component));
    }

    pub fn add_boxed(&mut self, component: Box<dyn RuntimeComponent>) {
        self.components.push(component);
    }

    /// Starts every registered component and coordinates one graceful shutdown.
    ///
    /// # Errors
    ///
    /// Returns the first component, join, signal, or graceful-timeout error.
    pub async fn run(self) -> anyhow::Result<()> {
        if self.components.is_empty() {
            return Err(anyhow!("no runtime component was enabled"));
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        for component in self.components {
            let name = component.name();
            let shutdown = Shutdown {
                receiver: shutdown_rx.clone(),
            };
            tasks.spawn(async move { component.run(shutdown).await.with_context(|| format!("{name} stopped")) });
        }

        let first_error = tokio::select! {
            result = ServerSupervisor::process_shutdown_signal() => {
                result?;
                tracing::info!("shutdown signal received");
                None
            }
            result = tasks.join_next() => {
                Some(match result {
                    Some(Ok(Ok(()))) => anyhow!("runtime component exited before shutdown"),
                    Some(Ok(Err(error))) => error,
                    Some(Err(error)) => anyhow!("runtime component task failed: {error}"),
                    None => anyhow!("no runtime component was started"),
                })
            }
        };
        let _ = shutdown_tx.send(true);
        let graceful = async {
            let mut error = first_error;
            while let Some(result) = tasks.join_next().await {
                let result =
                    result.unwrap_or_else(|join_error| Err(anyhow!("runtime component task failed: {join_error}")));
                if error.is_none() {
                    error = result.err();
                }
            }
            error
        };
        let Ok(error) = tokio::time::timeout(self.shutdown_timeout, graceful).await else {
            tasks.abort_all();
            return Err(anyhow!(
                "graceful shutdown exceeded {} seconds",
                self.shutdown_timeout.as_secs_f64()
            ));
        };
        error.map_or(Ok(()), Err)
    }
}

impl ServerSupervisor {
    async fn process_shutdown_signal() -> anyhow::Result<()> {
        let ctrl_c = async { tokio::signal::ctrl_c().await.context("install Ctrl+C handler") };
        #[cfg(unix)]
        let terminate = async {
            let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("install SIGTERM handler")?;
            let _ = signal.recv().await;
            Ok(())
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<anyhow::Result<()>>();

        tokio::select! {
            result = ctrl_c => result,
            result = terminate => result,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/lifecycle_test.rs"]
mod tests;
