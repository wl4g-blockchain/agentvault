use super::*;

#[test]
fn balance_snapshot_keeps_asset_and_native_requirements_separate() {
    let snapshot = WalletBalanceSnapshot {
        asset_available: Amount::new(10),
        native_available: Amount::new(2),
        minimum_native_required: Amount::new(1),
    };

    assert_eq!(snapshot.asset_available, Amount::new(10));
    assert!(snapshot.native_available >= snapshot.minimum_native_required);
    assert_eq!(WalletBalanceSyncRun::default().scanned, 0);
}
