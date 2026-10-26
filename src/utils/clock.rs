use crate::{Error, Result};

pub trait UnixClock: Send + Sync {
    /// Returns seconds since the Unix epoch.
    ///
    /// # Errors
    ///
    /// Returns an error if the host clock is before the Unix epoch or outside
    /// the persisted timestamp range.
    fn now(&self) -> Result<i64>;
}

#[derive(Debug, Default)]
pub(crate) struct SystemUnixClock;

impl UnixClock for SystemUnixClock {
    fn now(&self) -> Result<i64> {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Error::Settlement(format!("system clock is before Unix epoch: {error}")))?
            .as_secs();
        i64::try_from(seconds).map_err(|_| Error::Settlement("system clock exceeds timestamp range".to_owned()))
    }
}

#[cfg(test)]
#[path = "../../tests/utils/clock_test.rs"]
mod tests;
