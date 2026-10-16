mod memory;
#[path = "cache.rs"]
mod port;
mod redis;

pub use memory::MemoryCache;
pub use port::{CacheError, CacheFactory, CacheResult, ICache};
pub use redis::RedisCache;

#[cfg(test)]
#[path = "../../tests/cache/mod_test.rs"]
mod tests;
