use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn signing_contract(self) {
        let intent = self.challenge().await;
        assert!(matches!(
            self.ports.signing.load_for_principal(Some(""), &intent.id).await,
            Err(crate::Error::Unauthenticated)
        ));
        assert!(self
            .ports
            .signing
            .load_for_principal(Some("another-principal"), &intent.id)
            .await
            .unwrap()
            .is_none());
        // Inbound intents cannot enter the payer signing path, even for their owner.
        assert!(self
            .ports
            .signing
            .load_for_principal(Some(&self.client.auth_principal), &intent.id)
            .await
            .unwrap()
            .is_none());
        assert!(self
            .ports
            .wallets
            .load_active_key(&self.wallet.id)
            .await
            .unwrap()
            .is_none());
    }
}
