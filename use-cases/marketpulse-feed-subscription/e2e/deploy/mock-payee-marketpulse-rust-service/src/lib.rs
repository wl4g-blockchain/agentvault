pub mod handler;
pub mod route;
pub mod server;
pub mod storage;

use std::env;

/// Reads a mandatory non-blank environment variable.
///
/// # Errors
///
/// Returns an error when the variable is absent or blank.
pub fn required_env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = env::var(name).map_err(|_| format!("{name} is required"))?;
    if value.trim().is_empty() {
        return Err(format!("{name} must not be blank").into());
    }
    Ok(value)
}

#[cfg(test)]
#[path = "../tests/lib_test.rs"]
mod tests;
