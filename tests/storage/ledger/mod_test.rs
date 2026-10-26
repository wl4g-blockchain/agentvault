use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn ledger_contract(self) {
        use crate::{
            model::{Amount, LedgerEntryType, PaymentDirection, PaymentLedgerEntry},
            storage::{IdempotentWrite, LedgerQuery},
        };
        let entry = PaymentLedgerEntry {
            id: format!("deposit-{}", self.wallet.id),
            intent_id: None,
            scenario_id: Some(self.scenario.id),
            wallet_id: self.wallet.id.clone(),
            entry_type: LedgerEntryType::Deposit,
            direction: PaymentDirection::Inbound,
            asset: self.price.asset,
            amount: Amount::new(7),
            posting_key: format!("deposit-{}", self.wallet.id),
            tx_hash: None,
            settlement_ref: Some(format!("provider:{}", self.wallet.id)),
            effect_ref: None,
            transfer_ref: None,
            reversal_of: None,
            effective_at: 10,
            created_at: 10,
        };
        assert!(matches!(
            self.ports.ledger.append(&entry).await.unwrap(),
            IdempotentWrite::Inserted(_)
        ));
        assert_eq!(
            self.ports.ledger.append(&entry).await.unwrap(),
            IdempotentWrite::Replay(entry.clone())
        );
        let mut altered = entry.clone();
        altered.amount = Amount::new(8);
        assert!(self.ports.ledger.append(&altered).await.is_err());
        let page = self
            .ports
            .ledger
            .list(&LedgerQuery {
                wallet_id: Some(self.wallet.id),
                ..LedgerQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items, [entry]);
    }
}
