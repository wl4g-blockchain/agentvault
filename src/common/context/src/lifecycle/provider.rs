// SPDX-License-Identifier: GPL-3.0-or-later

//! Common lifecycle contract inherited by every managed domain interface.

use std::any::Any;

use anyhow::Result;
use async_trait::async_trait;

/// Common lifecycle for every provider.
///
/// `init` must be safe to retry after a failed or cancelled attempt. `close`
/// must work after partially completed `init`, be idempotent, and be retryable
/// after failure or cancellation. Capture configuration and dependencies in
/// the constructor registered with [`crate::ApplicationContext`].
/// `Any` enables typed retrieval and interface projections; every `'static`
/// implementor satisfies this bound automatically.
#[async_trait]
pub trait IProvider: Any + Send + Sync {
    /// Initializes the constructed instance before publishing it to callers.
    async fn init(&self) -> Result<()> {
        Ok(())
    }

    /// Releases resources, including those acquired by an incomplete `init`.
    async fn close(&self) -> Result<()>;
}
