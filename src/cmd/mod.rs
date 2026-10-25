mod agentvault;
mod console;

pub use agentvault::{AgentVaultServer, RuntimeComponent, RuntimeResource, ServerSupervisor, Shutdown};
pub use console::run as run_cli;

#[cfg(test)]
#[path = "../../tests/cmd/mod_test.rs"]
mod tests;
