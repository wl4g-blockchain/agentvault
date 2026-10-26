//! Native MPP challenge, reference and receipt projections.
use super::*;
use mpp::protocol::methods::tempo::TempoChargeExt as _;

pub(super) struct MppWire;
impl MppWire {
    #[cfg(test)]
    pub(super) fn official_challenge(challenge: &PaymentChallenge) -> Result<mpp::PaymentChallenge> {
        let header = challenge
            .payload
            .get("www_authenticate")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("stored MPP challenge is missing".to_owned()))?;
        mpp::parse_www_authenticate(header).map_err(MppWire::error)
    }

    pub(super) fn tempo_network(request: &ChargeRequest) -> Result<String> {
        let details = request
            .method_details
            .as_ref()
            .and_then(Value::as_object)
            .ok_or_else(|| {
                Error::PaymentProtocol("MPP Tempo methodDetails must be an object containing chainId".to_owned())
            })?;
        if details
            .keys()
            .any(|key| !matches!(key.as_str(), "chainId" | "supportedModes"))
        {
            return Err(Error::PaymentProtocol(
                "MPP Tempo split, fee-payer, machine-token, and delegated-key options are unsupported".to_owned(),
            ));
        }
        if let Some(modes) = details.get("supportedModes") {
            let pull_supported = modes.as_array().is_some_and(|modes| {
                !modes.is_empty()
                    && modes.iter().all(Value::is_string)
                    && modes.iter().any(|mode| mode.as_str() == Some("pull"))
            });
            if !pull_supported {
                return Err(Error::PaymentProtocol(
                    "MPP Tempo charge does not allow pull transaction credentials".to_owned(),
                ));
            }
        }
        let chain_id = request
            .chain_id()
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo chainId must be a numeric integer".to_owned()))?;
        Ok(format!("eip155:{chain_id}"))
    }

    pub(super) fn ensure_protocol(challenge: &PaymentChallenge) -> Result<()> {
        if challenge.protocol == MPP_PROTOCOL {
            Ok(())
        } else {
            Err(Error::PaymentProtocol("protocol adapter mismatch".to_owned()))
        }
    }

    pub(super) fn validate_resource(resource: &str) -> Result<()> {
        let parsed = reqwest::Url::parse(resource)
            .map_err(|_| Error::PaymentProtocol("payment resource is not an absolute URL".to_owned()))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(Error::PaymentProtocol("payment resource URL is unsafe".to_owned()));
        }
        Ok(())
    }

    pub(super) fn valid_evm_network(network: &str) -> bool {
        network
            .strip_prefix("eip155:")
            .is_some_and(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
    }

    pub(super) fn valid_transaction_hash(value: &str) -> bool {
        value.len() == 66 && value.starts_with("0x") && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    pub(super) fn decode_payment_reference(value: &str) -> Result<Vec<u8>> {
        value
            .strip_prefix("0x")
            .filter(|encoded| !encoded.is_empty() && encoded.len() % 2 == 0)
            .ok_or_else(|| Error::PaymentProtocol("MPP payment reference is not canonical hex".to_owned()))
            .and_then(|encoded| {
                hex::decode(encoded)
                    .map_err(|_| Error::PaymentProtocol("MPP payment reference is not canonical hex".to_owned()))
            })
    }

    pub(super) fn challenge_hash(resource: &str, header: &str) -> String {
        MppWire::hash_values(&[b"agentvault.mpp.challenge.v1", resource.as_bytes(), header.as_bytes()])
    }

    pub(super) fn hash_values(values: &[&[u8]]) -> String {
        let mut hash = Sha256::new();
        for value in values {
            hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            hash.update(value);
        }
        hex::encode(hash.finalize())
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn error(error: mpp::MppError) -> Error {
        Error::PaymentProtocol(error.to_string())
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/mpp_wire_test.rs"]
mod tests;
