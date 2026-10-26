use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn price_delete_contract(self) {
        assert!(self
            .ports
            .offers
            .delete(&self.offer.id, self.offer.updated_at - 1)
            .await
            .is_err());
        assert_eq!(
            self.ports.offers.get(&self.offer.id).await.unwrap(),
            Some(self.offer.clone())
        );
        self.ports
            .offers
            .delete(&self.offer.id, self.offer.updated_at)
            .await
            .unwrap();
        assert!(self.ports.offers.get(&self.offer.id).await.unwrap().is_none());
        self.ports.offers.insert(&self.offer).await.unwrap();
        let intent = self.challenge().await;
        assert!(self
            .ports
            .offers
            .delete(&self.offer.id, self.offer.updated_at)
            .await
            .is_err());
        assert_eq!(self.ports.offers.get(&self.offer.id).await.unwrap(), Some(self.offer));
        assert_eq!(self.ports.payments.get(&intent.id).await.unwrap(), Some(intent));
    }

    pub async fn offer_contract(self) {
        assert_eq!(
            self.ports
                .offers
                .resolve_resource(&self.scenario.id, &self.offer.resource)
                .await
                .unwrap(),
            Some(self.offer.clone())
        );
        let mut duplicate = self.offer.clone();
        duplicate.id.push_str("-duplicate");
        assert!(self.ports.offers.insert(&duplicate).await.is_err());
        let mut disabled = self.offer.clone();
        disabled.enabled = false;
        disabled.updated_at += 1;
        self.ports
            .offers
            .update(&disabled, self.offer.updated_at)
            .await
            .unwrap();
        assert!(self
            .ports
            .offers
            .resolve(&self.scenario.id, &self.offer.resource, "x402")
            .await
            .unwrap()
            .is_none());
        assert_eq!(self.ports.offers.get(&self.offer.id).await.unwrap(), Some(disabled));
    }
}
