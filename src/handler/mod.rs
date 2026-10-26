//! Domain application services. Integrations and payment execution live in their dedicated components.

mod approval;
mod client;
mod event;
mod intent;
mod ledger;
mod policy;
mod price;
mod scenario;
mod wallet;
pub use approval::ApprovalHandler;
pub use client::PayClientHandler;
pub use event::EventHandler;
pub use intent::IntentHandler;
pub use ledger::LedgerHandler;
pub use policy::PolicyHandler;
pub use price::PriceHandler;
pub use scenario::ScenarioHandler;
pub use wallet::WalletHandler;

#[cfg(test)]
#[path = "../../tests/handler/mod_test.rs"]
mod tests;
