use super::*;

#[test]
fn generates_and_validates_full_uuid_v4_nonces() {
    let first = UuidNonceCodec::generate();
    let second = UuidNonceCodec::generate();

    assert_eq!(UuidNonceCodec::parse(&first).unwrap(), first);
    assert_eq!(UuidNonceCodec::parse(&second).unwrap(), second);
    assert_ne!(first, second);
}

#[test]
fn rejects_non_uuid_or_non_v4_nonces() {
    assert!(UuidNonceCodec::parse(&[0_u8; UUID_NONCE_BYTES - 1]).is_err());
    assert!(UuidNonceCodec::parse(&[0_u8; UUID_NONCE_BYTES]).is_err());
}
