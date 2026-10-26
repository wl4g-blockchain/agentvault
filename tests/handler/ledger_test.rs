use super::*;
use crate::{
    model::{Amount, LedgerEntryType, PaymentDirection},
    storage::IdempotentWrite,
};

#[tokio::test]
async fn confirmed_financial_facts_replay_once_and_cannot_change_amount() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;

    let service = LedgerHandler::new(fixture.ports.ledger);
    let entry = PaymentLedgerEntry {
        id: "handler-deposit".into(),
        intent_id: None,
        scenario_id: Some(fixture.scenario.id),
        wallet_id: fixture.wallet.id,
        entry_type: LedgerEntryType::Deposit,
        direction: PaymentDirection::Inbound,
        asset: fixture.price.asset,
        amount: Amount::new(7),
        posting_key: "handler-deposit".into(),
        tx_hash: None,
        settlement_ref: Some("provider:handler-deposit".into()),
        effect_ref: None,
        transfer_ref: None,
        reversal_of: None,
        effective_at: 10,
        created_at: 10,
    };
    assert!(matches!(
        service.append(&entry).await.unwrap(),
        IdempotentWrite::Inserted(_)
    ));
    assert_eq!(
        service.append(&entry).await.unwrap(),
        IdempotentWrite::Replay(entry.clone())
    );
    let mut conflict = entry.clone();
    conflict.amount = Amount::new(8);
    assert!(service.append(&conflict).await.is_err());
    assert_eq!(service.get(&entry.id).await.unwrap(), Some(entry));
}
