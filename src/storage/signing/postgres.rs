use crate::storage::{
    payment::postgres::PaymentPostgresRepository as PaymentRepository,
    policy::postgres::PolicyPostgresRepository as PolicyRepository, postgres::BasePostgresRepository as BaseRepository,
    wallet::postgres::WalletPostgresRepository as WalletRepository,
};
use sqlx::PgConnection as Connection;

// Signing repository; transaction rules shared across SQLx drivers.

crate::storage::signing::transactions::signing_repository!(
    SigningPostgresRepository,
    " FOR UPDATE",
    " FOR SHARE",
    " FOR UPDATE SKIP LOCKED",
    " FOR UPDATE OF i",
    " FOR SHARE OF c",
    " FOR SHARE OF policy",
    " FOR SHARE OF c, s"
);

#[cfg(test)]
#[path = "../../../tests/storage/signing/postgres_test.rs"]
mod tests;

#[derive(Clone)]
pub struct SigningPostgresRepository {
    base: BaseRepository,
}
impl SigningPostgresRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
