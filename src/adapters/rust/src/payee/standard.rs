use crate::{PayeeMiddleware, PayeeOperation, ProtectionOutcome, Result, VaultTransport};

#[derive(Clone, Debug, PartialEq)]
pub struct StandardPayeeRequest {
    pub operation: PayeeOperation,
}

/// Framework-neutral adapter for custom Rust HTTP/MCP servers.
#[derive(Clone)]
pub struct StandardPayeeMiddleware<T> {
    core: PayeeMiddleware<T>,
}

impl<T> StandardPayeeMiddleware<T> {
    #[must_use]
    pub const fn new(core: PayeeMiddleware<T>) -> Self {
        Self { core }
    }
}

impl<T: VaultTransport> StandardPayeeMiddleware<T> {
    pub async fn handle(&self, request: &StandardPayeeRequest) -> Result<ProtectionOutcome> {
        self.core.protect(&request.operation).await
    }
}

#[cfg(test)]
#[path = "../../tests/payee/standard_test.rs"]
mod tests;
