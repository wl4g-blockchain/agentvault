use k256::ecdsa::{SigningKey, VerifyingKey};
use rand_core::OsRng;
use sha3::{Digest, Keccak256};
use zeroize::Zeroizing;

use crate::{Error, Result};

pub const PRIVATE_KEY_BYTES: usize = 32;
pub const DIGEST_BYTES: usize = 32;
pub const EOA_SIGNATURE_BYTES: usize = 65;

pub fn generate_private_key() -> Zeroizing<Vec<u8>> {
    Zeroizing::new(SigningKey::random(&mut OsRng).to_bytes().to_vec())
}

/// Validates a raw secp256k1 private key.
///
/// # Errors
///
/// Returns an error for a key with the wrong length or scalar value.
pub fn validate_private_key(private_key: &[u8]) -> Result<()> {
    if private_key.len() != PRIVATE_KEY_BYTES {
        return Err(Error::InvalidKey(format!(
            "secp256k1 private key must be {PRIVATE_KEY_BYTES} bytes"
        )));
    }
    SigningKey::from_slice(private_key)
        .map(|_| ())
        .map_err(|error| Error::InvalidKey(error.to_string()))
}

/// Derives the lower-case Ethereum EOA address for a private key.
///
/// # Errors
///
/// Returns an error when the private key is invalid.
pub fn address(private_key: &[u8]) -> Result<String> {
    let signing_key = signing_key(private_key)?;
    Ok(address_from_verifying_key(signing_key.verifying_key()))
}

/// Derives the uncompressed secp256k1 public key.
///
/// # Errors
///
/// Returns an error when the private key is invalid.
pub fn public_key(private_key: &[u8]) -> Result<String> {
    let signing_key = signing_key(private_key)?;
    Ok(format!(
        "0x{}",
        hex::encode(signing_key.verifying_key().to_encoded_point(false).as_bytes())
    ))
}

/// Signs a precomputed 32-byte digest and returns Ethereum `r || s || v` bytes.
///
/// # Errors
///
/// Returns an error when the key or digest is invalid or signing fails.
pub fn sign_digest(private_key: &[u8], digest: &[u8]) -> Result<[u8; EOA_SIGNATURE_BYTES]> {
    if digest.len() != DIGEST_BYTES {
        return Err(Error::InvalidRequest(format!(
            "EIP-712 digest must be {DIGEST_BYTES} bytes"
        )));
    }
    let signing_key = signing_key(private_key)?;
    let (signature, recovery_id) = signing_key
        .sign_prehash_recoverable(digest)
        .map_err(|error| Error::Crypto(error.to_string()))?;

    let mut output = [0_u8; EOA_SIGNATURE_BYTES];
    output[..64].copy_from_slice(&signature.to_bytes());
    output[64] = recovery_id.to_byte() + 27;
    Ok(output)
}

fn signing_key(private_key: &[u8]) -> Result<SigningKey> {
    validate_private_key(private_key)?;
    SigningKey::from_slice(private_key).map_err(|error| Error::InvalidKey(error.to_string()))
}

fn address_from_verifying_key(verifying_key: &VerifyingKey) -> String {
    let encoded = verifying_key.to_encoded_point(false);
    let hash = Keccak256::digest(&encoded.as_bytes()[1..]);
    format!("0x{}", hex::encode(&hash[12..]))
}

#[cfg(test)]
mod tests {
    use k256::ecdsa::{RecoveryId, Signature};

    use super::*;

    #[test]
    fn generated_key_signs_recoverable_digest() {
        let private_key = generate_private_key();
        let digest = Keccak256::digest(b"wallet-test");
        let signature = sign_digest(&private_key, &digest).unwrap();
        assert!(matches!(signature[64], 27 | 28));

        let compact = Signature::try_from(&signature[..64]).unwrap();
        let recovery_id = RecoveryId::try_from(signature[64] - 27).unwrap();
        let recovered = VerifyingKey::recover_from_prehash(&digest, &compact, recovery_id).unwrap();
        assert_eq!(address(&private_key).unwrap(), address_from_verifying_key(&recovered));
    }

    #[test]
    fn rejects_non_32_byte_digest() {
        let private_key = generate_private_key();
        assert!(sign_digest(&private_key, b"not-a-digest").is_err());
    }
}
