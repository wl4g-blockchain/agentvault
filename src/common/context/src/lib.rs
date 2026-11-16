// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed registration and lazy lifecycle management for application providers.
//!
//! Domain interfaces inherit [`IProvider`]. Register constructors that capture
//! their configuration and dependencies, then retrieve instances by the domain
//! interface type and instance ID. Dependencies must be acyclic; assign
//! dependents higher shutdown phases so they close before their dependencies.
//! Register another interface of the same object with
//! [`ApplicationContext::register_with`] to share one initialization and close.
//!
//! Application modules obtain the same context from [`ApplicationContext::global`].
//! The entry point registers providers, starts the context and shuts it down.
//! Registration uses the domain interface type and a single instance ID:
//!
//! ```
//! # use std::sync::Arc;
//! # use anyhow::Result;
//! # use async_trait::async_trait;
//! # use common_context::{ApplicationContext, IProvider};
//! # struct IngestedRecord { source_id: String, text: String }
//! # trait ITestIngestor: IProvider {
//! #     fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord;
//! # }
//! # struct WebTestIngestor;
//! # struct TwitterTestIngestor;
//! # #[async_trait]
//! # impl IProvider for WebTestIngestor {
//! #     async fn close(&self) -> Result<()> { Ok(()) }
//! # }
//! # impl ITestIngestor for WebTestIngestor {
//! #     fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord {
//! #         IngestedRecord { source_id: source_id.to_owned(), text: content.split_whitespace().collect::<Vec<_>>().join(" ") }
//! #     }
//! # }
//! # #[async_trait]
//! # impl IProvider for TwitterTestIngestor {
//! #     async fn close(&self) -> Result<()> { Ok(()) }
//! # }
//! # impl ITestIngestor for TwitterTestIngestor {
//! #     fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord {
//! #         IngestedRecord { source_id: source_id.to_owned(), text: content.trim().to_owned() }
//! #     }
//! # }
//! # #[tokio::main]
//! # async fn main() -> Result<()> {
//! # let web_ingestor_instantiator = || async { Ok(Arc::new(WebTestIngestor) as Arc<dyn ITestIngestor>) };
//! # let twitter_ingestor_instantiator = || async { Ok(Arc::new(TwitterTestIngestor) as Arc<dyn ITestIngestor>) };
//! let ctx = ApplicationContext::global();
//! ctx.register::<dyn ITestIngestor, _>("web-default", web_ingestor_instantiator)?;
//! ctx.register::<dyn ITestIngestor, _>("twitter-default", twitter_ingestor_instantiator)?;
//! ctx.init().await?;
//! let web = ctx.get::<dyn ITestIngestor>("web-default").await?;
//! let twitter = ctx.get::<dyn ITestIngestor>("twitter-default").await?;
//! let page = web.ingest("https://example.test/news/42", "  Market   update  ");
//! let tweet = twitter.ingest("tweet-42", "  Latest update  ");
//! # assert_eq!(page.source_id, "https://example.test/news/42");
//! # assert_eq!(page.text, "Market update");
//! # assert_eq!(tweet.source_id, "tweet-42");
//! # assert_eq!(tweet.text, "Latest update");
//! ctx.close_for::<dyn ITestIngestor>("web-default").await?;
//! ctx.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! [`ApplicationContext::new`] creates an independent context for tests or
//! separately managed applications. The following isolated example exposes
//! ingestion metrics through another interface of the same Twitter ingestor.
//! Fixed input payloads keep the example deterministic:
//!
//! ```
//! use std::{any::Any, sync::{Arc, Mutex}};
//! use anyhow::Result;
//! use async_trait::async_trait;
//! use common_context::{ApplicationContext, IProvider};
//!
//! #[derive(Clone)]
//! struct IngestedRecord { source_id: String, text: String }
//!
//! trait ITestIngestor: IProvider {
//!     fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord;
//! }
//!
//! trait ITestIngestorMetrics: IProvider {
//!     fn ingested_count(&self) -> usize;
//! }
//!
//! #[derive(Default)]
//! struct TwitterTestIngestor(Mutex<Vec<IngestedRecord>>);
//!
//! #[async_trait]
//! impl IProvider for TwitterTestIngestor {
//!     async fn close(&self) -> Result<()> {
//!         self.0.lock().unwrap().clear();
//!         Ok(())
//!     }
//! }
//!
//! impl ITestIngestor for TwitterTestIngestor {
//!     fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord {
//!         let record = IngestedRecord { source_id: source_id.to_owned(), text: content.trim().to_owned() };
//!         self.0.lock().unwrap().push(record.clone());
//!         record
//!     }
//! }
//!
//! impl ITestIngestorMetrics for TwitterTestIngestor {
//!     fn ingested_count(&self) -> usize { self.0.lock().unwrap().len() }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<()> {
//! let context = ApplicationContext::new();
//! context.register::<dyn ITestIngestor, _>("twitter-default", || async {
//!     Ok(Arc::new(TwitterTestIngestor::default()) as Arc<dyn ITestIngestor>)
//! })?;
//! context.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("twitter-default", |ingestor| {
//!     let object: Arc<dyn Any + Send + Sync> = ingestor;
//!     let ingestor = object.downcast::<TwitterTestIngestor>()
//!         .map_err(|_| anyhow::anyhow!("unexpected ingestor implementation"))?;
//!     Ok(ingestor as Arc<dyn ITestIngestorMetrics>)
//! })?;
//! context.init().await?;
//! let ingestor = context.get::<dyn ITestIngestor>("twitter-default").await?;
//! let record = ingestor.ingest("tweet-42", "  Market update  ");
//! assert_eq!(record.text, "Market update");
//! let metrics = context.get::<dyn ITestIngestorMetrics>("twitter-default").await?;
//! assert_eq!(metrics.ingested_count(), 1);
//! context.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Types without the top-level lifecycle interface cannot be registered:
//!
//! ```compile_fail
//! use std::sync::Arc;
//! use common_context::ApplicationContext;
//!
//! struct Unmanaged;
//!
//! ApplicationContext::new().register::<Unmanaged, _>("default", || async {
//!     Ok(Arc::new(Unmanaged))
//! }).unwrap();
//! ```

/// Shared provider contracts and application lifecycle management.
pub mod lifecycle {
    pub mod application;
    pub mod provider;
}

/// Cross-project process utilities owned by the application lifecycle layer.
pub mod util;

pub use lifecycle::application::ApplicationContext;

pub use lifecycle::provider::IProvider;
