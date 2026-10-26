use crate::model::{Amount, BudgetUnit, Wallet};
use crate::storage::*;

fn payment_wallet() -> PaymentWallet {
    PaymentWallet {
        wallet: Wallet {
            id: "wallet-1".to_owned(),
            algorithm: "external".to_owned(),
            address: "opaque-recipient".to_owned(),
            public_key: String::new(),
            created_at: 1,
        },
        custody: WalletCustody::External,
        enabled: true,
        active_key_version: None,
        budget_unit: None,
        max_per_payment: None,
        max_per_day: None,
        updated_at: 1,
    }
}

#[test]
fn unbound_wallet_needs_no_budget_unit() {
    assert!(payment_wallet().validate_payment_metadata().is_ok());
}

#[test]
fn scalar_wallet_limits_require_one_valid_unit() {
    let mut wallet = payment_wallet();
    wallet.max_per_day = Some(Amount::new(100));
    assert_eq!(wallet.validate_payment_metadata(), Err("budget_unit"));

    wallet.budget_unit = Some(BudgetUnit {
        asset: "USDC".to_owned(),
        decimals: 6,
    });
    wallet.max_per_payment = Some(Amount::new(101));
    assert_eq!(wallet.validate_payment_metadata(), Err("max_per_day"));

    wallet.max_per_payment = Some(Amount::new(50));
    assert!(wallet.validate_payment_metadata().is_ok());
}

#[test]
fn only_local_wallet_with_active_key_is_signing_ready() {
    let mut wallet = payment_wallet();
    assert!(!wallet.signing_ready());
    wallet.active_key_version = Some(1);
    assert_eq!(wallet.validate_payment_metadata(), Err("active_key_version"));
    wallet.custody = WalletCustody::Local;
    assert!(wallet.signing_ready());
    assert!(wallet.validate_payment_metadata().is_ok());
    wallet.enabled = false;
    assert!(!wallet.signing_ready());
}

#[test]
fn local_wallet_must_match_payment_network_before_signing() {
    let mut wallet = payment_wallet();
    wallet.enabled = true;
    wallet.custody = WalletCustody::Local;
    wallet.active_key_version = Some(1);
    wallet.wallet.algorithm = "secp256k1".to_owned();
    wallet.wallet.address = "0xabcdef0123456789abcdef0123456789abcdef01".to_owned();
    assert!(wallet.signing_ready_for_payment_network("eip155:8453"));

    wallet.wallet.algorithm = "ed25519".to_owned();
    assert!(!wallet.signing_ready_for_payment_network("eip155:8453"));
}
