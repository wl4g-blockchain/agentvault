use k256::ecdsa::{SigningKey, VerifyingKey};
use sha3::{Digest, Keccak256};
use solana_keypair::Keypair;
use solana_signer::Signer as _;

#[cfg(test)]
use rand_core::OsRng;
#[cfg(test)]
use zeroize::Zeroizing;

use crate::{
    model::wallet::{is_evm_wallet_algorithm, is_solana_wallet_algorithm},
    Error, Result,
};

pub const PRIVATE_KEY_BYTES: usize = 32;

pub struct WalletCrypto;

const EIP712_DIGEST_BYTES: usize = 32;
pub const EOA_SIGNATURE_BYTES: usize = 65;
pub const SOLANA_SIGNATURE_BYTES: usize = 64;

impl WalletCrypto {
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
        let signing_key = WalletCrypto::signing_key(private_key)?;
        Ok(WalletCrypto::address_from_verifying_key(signing_key.verifying_key()))
    }

    /// Derives the uncompressed secp256k1 public key.
    ///
    /// # Errors
    ///
    /// Returns an error when the private key is invalid.
    pub fn public_key(private_key: &[u8]) -> Result<String> {
        let signing_key = WalletCrypto::signing_key(private_key)?;
        Ok(format!(
            "0x{}",
            hex::encode(signing_key.verifying_key().to_encoded_point(false).as_bytes())
        ))
    }

    /// Validates private key material for a supported local custody algorithm.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported algorithm or malformed key material.
    pub fn validate_private_key_for_algorithm(algorithm: &str, private_key: &[u8]) -> Result<()> {
        if is_evm_wallet_algorithm(algorithm) {
            return WalletCrypto::validate_private_key(private_key);
        }
        if is_solana_wallet_algorithm(algorithm) {
            WalletCrypto::solana_keypair(private_key)?;
            return Ok(());
        }
        Err(Error::InvalidKey("unsupported local wallet algorithm".to_owned()))
    }

    /// Derives the canonical address and public key using the official key type
    /// for the configured custody algorithm.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported algorithms or malformed key material.
    pub fn payment_key_metadata(algorithm: &str, private_key: &[u8]) -> Result<(String, String)> {
        if is_evm_wallet_algorithm(algorithm) {
            return Ok((
                WalletCrypto::address(private_key)?,
                WalletCrypto::public_key(private_key)?,
            ));
        }
        if is_solana_wallet_algorithm(algorithm) {
            let address = WalletCrypto::solana_keypair(private_key)?.pubkey().to_string();
            return Ok((address.clone(), address));
        }
        Err(Error::InvalidKey("unsupported local wallet algorithm".to_owned()))
    }

    /// Signs an intent-authorized EIP-712 digest using the official secp256k1 key.
    /// This primitive is crate-private; custody validates the intent before calling it.
    ///
    /// # Errors
    /// Returns an error for invalid keys, digest length or signing failure.
    pub(crate) fn sign_eip712_digest(private_key: &[u8], digest: &[u8]) -> Result<[u8; EOA_SIGNATURE_BYTES]> {
        if digest.len() != EIP712_DIGEST_BYTES {
            return Err(Error::InvalidRequest(format!(
                "EIP-712 digest must be {EIP712_DIGEST_BYTES} bytes"
            )));
        }
        let signing_key = WalletCrypto::signing_key(private_key)?;
        let (signature, recovery_id) = signing_key
            .sign_prehash_recoverable(digest)
            .map_err(|error| Error::Crypto(error.to_string()))?;

        let mut output = [0_u8; EOA_SIGNATURE_BYTES];
        output[..64].copy_from_slice(&signature.to_bytes());
        output[64] = recovery_id.to_byte() + 27;
        Ok(output)
    }

    /// Signs protocol-owned Solana transaction message bytes with the official
    /// Solana keypair implementation.
    pub(crate) fn sign_solana_message(private_key: &[u8], message: &[u8]) -> Result<[u8; SOLANA_SIGNATURE_BYTES]> {
        if message.is_empty() {
            return Err(Error::InvalidRequest("Solana signing message is empty".to_owned()));
        }
        Ok(WalletCrypto::solana_keypair(private_key)?.sign_message(message).into())
    }

    fn solana_keypair(private_key: &[u8]) -> Result<Keypair> {
        let seed: [u8; PRIVATE_KEY_BYTES] = private_key
            .try_into()
            .map_err(|_| Error::InvalidKey(format!("ed25519 private key seed must be {PRIVATE_KEY_BYTES} bytes")))?;
        Ok(Keypair::new_from_array(seed))
    }

    fn signing_key(private_key: &[u8]) -> Result<SigningKey> {
        WalletCrypto::validate_private_key(private_key)?;
        SigningKey::from_slice(private_key).map_err(|error| Error::InvalidKey(error.to_string()))
    }

    fn address_from_verifying_key(verifying_key: &VerifyingKey) -> String {
        let encoded = verifying_key.to_encoded_point(false);
        let hash = Keccak256::digest(&encoded.as_bytes()[1..]);
        format!("0x{}", hex::encode(&hash[12..]))
    }
}

#[cfg(test)]
#[path = "../../tests/utils/crypto_test.rs"]
mod tests;
