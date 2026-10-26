use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn client_contract(self) {
        let fetched = self
            .ports
            .clients
            .find_by_principal(&self.client.auth_principal)
            .await
            .unwrap();
        assert_eq!(fetched, Some(self.client.clone()));
        let mut collision = self.client.clone();
        collision.id.push_str("-collision");
        assert!(self.ports.clients.insert(&collision).await.is_err());
        self.ports.clients.set_enabled(&self.client.id, false, 2).await.unwrap();
        assert!(!self.ports.clients.get(&self.client.id).await.unwrap().unwrap().enabled);
        assert!(self.ports.clients.set_enabled(&self.client.id, true, 1).await.is_err());
    }
}
