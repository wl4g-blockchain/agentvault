use super::*;

#[test]
fn generates_and_validates_full_uuid_v4_nonces() {
    let first = new_uuid_nonce();
    let second = new_uuid_nonce();

    assert_eq!(parse_uuid_nonce(&first).unwrap(), first);
    assert_eq!(parse_uuid_nonce(&second).unwrap(), second);
    assert_ne!(first, second);
}

#[test]
fn rejects_non_uuid_or_non_v4_nonces() {
    assert!(parse_uuid_nonce(&[0_u8; UUID_NONCE_BYTES - 1]).is_err());
    assert!(parse_uuid_nonce(&[0_u8; UUID_NONCE_BYTES]).is_err());
}
