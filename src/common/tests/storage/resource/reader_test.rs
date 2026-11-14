use super::*;
use std::os::unix::fs::symlink;
use tempfile::TempDir;
#[test]
fn flattened_wallet_source_rejects_inline_keys_and_unknown_fields() {
    let temp = TempDir::new().unwrap();
    for body in [
        "consoleVersion: core.agentvault.io/v1\nkind: Wallet\nmetadata: { name: wallet }\ndata: { custody: local, private_key_env: KEY_ENV, private_key: plaintext-is-forbidden }\n",
        "consoleVersion: core.agentvault.io/v1\nkind: Wallet\nmetadata: { name: wallet }\ndata: { custody: external, algorithm: external, address: recipient, unexpected: true }\n",
    ] {
        let resource = temp.path().join(format!("wallet-{}.yaml", uuid::Uuid::new_v4()));
        std::fs::write(&resource, body).unwrap();
        assert!(ResourceImporter::read_resources(&[resource]).is_err());
    }
}

#[test]
fn resource_reader_rejects_symbolic_links() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("resource-target.yaml");
    let link = temp.path().join("resource-link.yaml");
    std::fs::write(&target, "consoleVersion: core.agentvault.io/v1\nkind: Client\nmetadata: { name: client }\ndata: { name: Client, auth_principal: principal }\n").unwrap();
    symlink(&target, &link).unwrap();

    assert!(ResourceImporter::read_resources(&[link]).is_err());
    assert_eq!(ResourceImporter::read_resources(&[target]).unwrap().clients.len(), 1);
}
