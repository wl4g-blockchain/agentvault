#[allow(clippy::module_inception)]
mod config;

pub use config::*;

#[cfg(test)]
#[path = "../../tests/config/mod_test.rs"]
mod tests;
