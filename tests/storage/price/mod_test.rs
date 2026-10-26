use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn price_delete_contract(self) {
        assert!(self
            .ports
            .prices
            .delete(&self.price.id, self.price.updated_at - 1)
            .await
            .is_err());
        assert_eq!(
            self.ports.prices.get(&self.price.id).await.unwrap(),
            Some(self.price.clone())
        );
        self.ports
            .prices
            .delete(&self.price.id, self.price.updated_at)
            .await
            .unwrap();
        assert!(self.ports.prices.get(&self.price.id).await.unwrap().is_none());
        self.ports.prices.insert(&self.price).await.unwrap();
        let intent = self.challenge().await;
        assert!(self
            .ports
            .prices
            .delete(&self.price.id, self.price.updated_at)
            .await
            .is_err());
        assert_eq!(self.ports.prices.get(&self.price.id).await.unwrap(), Some(self.price));
        assert_eq!(self.ports.payments.get(&intent.id).await.unwrap(), Some(intent));
    }

    pub async fn price_contract(self) {
        assert_eq!(
            self.ports
                .prices
                .resolve_resource(&self.scenario.id, &self.price.resource)
                .await
                .unwrap(),
            Some(self.price.clone())
        );
        let mut duplicate = self.price.clone();
        duplicate.id.push_str("-duplicate");
        assert!(self.ports.prices.insert(&duplicate).await.is_err());
        let mut disabled = self.price.clone();
        disabled.enabled = false;
        disabled.updated_at += 1;
        self.ports
            .prices
            .update(&disabled, self.price.updated_at)
            .await
            .unwrap();
        assert!(self
            .ports
            .prices
            .resolve(&self.scenario.id, &self.price.resource, "x402")
            .await
            .unwrap()
            .is_none());
        assert_eq!(self.ports.prices.get(&self.price.id).await.unwrap(), Some(disabled));
    }
}
