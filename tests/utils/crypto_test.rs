use k256::ecdsa::{RecoveryId, Signature};

use super::*;

#[test]
fn generated_key_signs_recoverable_digest() {
    let private_key = generate_private_key();
    let digest = Keccak256::digest(b"wallet-test");
    let signature = sign_eip712_digest(&private_key, &digest).unwrap();
    assert!(matches!(signature[64], 27 | 28));

    let compact = Signature::try_from(&signature[..64]).unwrap();
    let recovery_id = RecoveryId::try_from(signature[64] - 27).unwrap();
    let recovered = VerifyingKey::recover_from_prehash(&digest, &compact, recovery_id).unwrap();
    assert_eq!(address(&private_key).unwrap(), address_from_verifying_key(&recovered));
}

#[test]
fn rejects_non_32_byte_digest() {
    let private_key = generate_private_key();
    assert!(sign_eip712_digest(&private_key, b"not-a-digest").is_err());
}
