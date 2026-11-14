use super::*;

#[test]
fn seals_with_context_and_opens_retired_key_versions() {
    let first = Aes256GcmMaterialProtector::new("v1", [("v1".to_owned(), [1_u8; 32])]).unwrap();
    let sealed = first.seal("intent:1:x402", b"credential").unwrap();
    assert!(!sealed.ciphertext.contains("credential"));
    assert_eq!(sealed.version, 2);
    assert_eq!(sealed.nonce.len(), 22);

    let rotated =
        Aes256GcmMaterialProtector::new("v2", [("v1".to_owned(), [1_u8; 32]), ("v2".to_owned(), [2_u8; 32])]).unwrap();
    assert_eq!(
        rotated.open("intent:1:x402", &sealed).unwrap().as_slice(),
        b"credential"
    );
    assert!(rotated.open("intent:2:x402", &sealed).is_err());
}
