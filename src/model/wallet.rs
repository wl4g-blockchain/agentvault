use serde::{Deserialize, Serialize};

use crate::store::KeyInfo;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWalletRequest {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Wallet {
    pub id: String,
    pub algorithm: String,
    pub address: String,
    pub public_key: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WalletList {
    pub items: Vec<Wallet>,
    pub total: usize,
}

impl From<KeyInfo> for Wallet {
    fn from(value: KeyInfo) -> Self {
        Self {
            id: value.id,
            algorithm: value.algorithm,
            address: value.address,
            public_key: value.public_key,
            created_at: value.created_at,
        }
    }
}
