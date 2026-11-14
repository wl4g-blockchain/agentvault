use super::*;
use crate::model::resource::ClientSpec;
#[test]
fn client_manifest_cannot_bind_two_clients_to_one_principal() {
    let mut batch = ImportBatch::default();
    for id in ["client-a", "client-b"] {
        batch.clients.push(ClientSpec {
            id: id.into(),
            name: id.into(),
            auth_principal: "principal".into(),
            enabled: true,
        });
    }
    assert!(ResourceImporter::validate_batch(&batch).is_err());
}
