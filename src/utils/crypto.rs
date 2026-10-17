use k256::ecdsa::{SigningKey, VerifyingKey};
use sha3::{Digest, Keccak256};

#[cfg(test)]
use rand_core::OsRng;
#[cfg(test)]
use zeroize::Zeroizing;

use crate::{Error, Result};

pub const PRIVATE_KEY_BYTES: usize = 32;
const EIP712_DIGEST_BYTES: usize = 32;
pub const EOA_SIGNATURE_BYTES: usize = 65;

#[cfg(test)]
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
/// Signs an EIP-712 prehash with an EOA key.
///
/// This is a cryptographic primitive, not an authorization endpoint. Runtime
/// callers must invoke it only after an intent-bound payment policy decision,
/// budget reservation, balance check, and approval-grant consumption.
///
/// # Errors
///
/// Returns an error when the key or EIP-712 digest is invalid or signing fails.
#[allow(dead_code)] // Kept crate-private for the intent-bound SQL signer composition.
pub(crate) fn sign_eip712_digest(private_key: &[u8], digest: &[u8]) -> Result<[u8; EOA_SIGNATURE_BYTES]> {
    if digest.len() != EIP712_DIGEST_BYTES {
        return Err(Error::InvalidRequest(format!(
            "EIP-712 digest must be {EIP712_DIGEST_BYTES} bytes"
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
#[path = "../../tests/utils/crypto_test.rs"]
mod tests;
