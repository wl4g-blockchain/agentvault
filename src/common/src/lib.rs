pub mod apm;
pub mod cache;
#[path = "config/config.rs"]
pub mod config;
pub mod model;
pub mod storage;
pub mod util;
pub use config::AppConfig;
pub use util::Error;
pub use util::Result;
#[allow(clippy::default_trait_access, clippy::doc_markdown)]
pub(crate) mod authguard_access_wire {
    tonic::include_proto!("authguard.access.v1");
}
