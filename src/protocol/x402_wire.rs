//! Strict x402 v2 wire projections and durable challenge identity.
use super::*;

pub(crate) struct X402Wire;

impl X402Adapter {
    pub(super) fn registered_asset(&self, network: &str, contract: &str) -> Result<AssetIdentity> {
        let registered = self
            .assets
            .iter()
            .find(|asset| asset.network == network && payment_recipients_match(network, &asset.contract, contract))
            .ok_or_else(|| Error::PaymentProtocol("x402 asset or network is not registered".to_owned()))?;
        Ok(AssetIdentity {
            network: registered.network.clone(),
            asset: registered.symbol.clone(),
            token_contract: Some(registered.contract.clone()),
            decimals: registered.decimals,
        })
    }
}

impl X402Adapter {
    pub(super) fn header_value<'a>(headers: &'a BTreeMap<String, String>, wanted: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_str())
    }
}

impl X402Adapter {
    pub(super) fn decode_payment_required_header(encoded: &str) -> Result<Value> {
        if encoded.is_empty()
            || encoded.len() > MAX_X402_HTTP_WIRE_BYTES
            || !encoded.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(Error::PaymentProtocol(
                "x402 PAYMENT-REQUIRED header is invalid".to_owned(),
            ));
        }
        let decoded = Base64Bytes::from(encoded.as_bytes())
            .decode()
            .map_err(|_| Error::PaymentProtocol("x402 PAYMENT-REQUIRED is not valid base64".to_owned()))?;
        if decoded.is_empty() || decoded.len() > MAX_X402_HTTP_WIRE_BYTES {
            return Err(Error::PaymentProtocol(
                "x402 PAYMENT-REQUIRED payload is too large".to_owned(),
            ));
        }
        serde_json::from_slice(&decoded)
            .map_err(|error| Error::PaymentProtocol(format!("invalid x402 PAYMENT-REQUIRED JSON: {error}")))
    }
}

impl X402Adapter {
    pub(super) fn decode_payment_required_body(body: &str) -> Result<Option<Value>> {
        if body.trim().is_empty() {
            return Ok(None);
        }
        if body.len() > MAX_X402_HTTP_WIRE_BYTES {
            return Err(Error::PaymentProtocol("x402 402 body is too large".to_owned()));
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Ok(None);
        };
        let declared = value.as_object().is_some_and(|object| {
            object.contains_key("x402Version") && object.get("accepts").is_some_and(Value::is_array)
        });
        Ok(declared.then_some(value))
    }
}

impl X402Adapter {
    pub(super) fn requirement(&self, wire: &X402PaymentRequired) -> Result<(X402Requirement, Value)> {
        for requirement in &wire.accepts {
            let value: Value = serde_json::from_str(requirement.0.get())
                .map_err(|error| Error::PaymentProtocol(format!("invalid x402 requirement JSON: {error}")))?;
            if let Ok(requirement) = self.parse_requirement(&value) {
                return Ok((requirement, value));
            }
        }
        Err(Error::PaymentProtocol("no supported x402 exact requirement".to_owned()))
    }
}

impl X402Adapter {
    pub(super) fn parse_requirement(&self, wire: &Value) -> Result<X402Requirement> {
        let network = wire
            .get("network")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("x402 requirement network is missing".to_owned()))?;
        let requirement = if X402Evm::valid_network(network) {
            X402Requirement::Evm(
                serde_json::from_value(wire.clone())
                    .map_err(|error| Error::PaymentProtocol(format!("invalid x402 EVM requirement: {error}")))?,
            )
        } else if X402Solana::valid_network(network) {
            X402Requirement::Solana(
                serde_json::from_value(wire.clone())
                    .map_err(|error| Error::PaymentProtocol(format!("invalid x402 Solana requirement: {error}")))?,
            )
        } else {
            return Err(Error::PaymentProtocol(
                "unsupported x402 requirement network".to_owned(),
            ));
        };
        X402Wire::validate_requirement(&requirement, self.max_timeout_seconds)?;
        self.registered_asset(&requirement.network(), &requirement.asset())?;
        if let X402Requirement::Solana(requirement) = &requirement {
            let runtime = self
                .solana
                .get(requirement.network.to_string().as_str())
                .ok_or_else(|| Error::PaymentProtocol("x402 Solana runtime is not configured".to_owned()))?;
            if runtime.fee_payer != requirement.extra.fee_payer {
                return Err(Error::PaymentProtocol(
                    "x402 Solana requirement fee payer is not registered".to_owned(),
                ));
            }
        }
        Ok(requirement)
    }
}

impl X402Adapter {
    pub(super) fn decode_challenge_wire(
        &self,
        expected_resource: &str,
        raw_wire: &Value,
        stored_selected: Option<&Value>,
    ) -> Result<X402ChallengeIntegrity> {
        X402Wire::validate_resource(expected_resource)?;
        let payment_required: X402PaymentRequired = serde_json::from_value(raw_wire.clone())
            .map_err(|error| Error::PaymentProtocol(format!("invalid x402 v2 challenge: {error}")))?;
        let resource = payment_required
            .resource
            .as_ref()
            .ok_or_else(|| Error::PaymentProtocol("x402 resource metadata is required".to_owned()))?;
        if resource.url != expected_resource {
            return Err(Error::PaymentProtocol(
                "x402 version or resource binding does not match the request".to_owned(),
            ));
        }

        let (selected, selected_wire) = match stored_selected {
            Some(selected) => (self.parse_requirement(selected)?, selected.clone()),
            None => self.requirement(&payment_required)?,
        };
        let advertised = payment_required.accepts.iter().any(|requirement| {
            serde_json::from_str::<Value>(requirement.0.get()).is_ok_and(|requirement| {
                X402Wire::canonical_json(&requirement) == X402Wire::canonical_json(&selected_wire)
            })
        });
        if !advertised {
            return Err(Error::PaymentProtocol(
                "x402 selected requirement is not advertised by PaymentRequired".to_owned(),
            ));
        }
        X402Wire::validate_requirement(&selected, self.max_timeout_seconds)?;

        let network = selected.network();
        let asset = self.registered_asset(&network, &selected.asset())?;
        let amount = selected.amount()?;
        let recipient = canonicalize_payment_recipient(&network, &selected.pay_to())
            .map_err(|_| Error::PaymentProtocol("x402 recipient is invalid for its network".to_owned()))?;
        Ok(X402ChallengeIntegrity {
            payment_required,
            selected,
            selected_wire,
            asset,
            amount,
            recipient,
        })
    }
}

impl X402Adapter {
    /// Reconstructs every security-sensitive field from the stored wire value.
    /// The protocol-neutral challenge is a cache of those fields, never a
    /// second source of truth.
    pub(super) fn validate_challenge_integrity(&self, challenge: &PaymentChallenge) -> Result<X402ChallengeIntegrity> {
        X402Wire::ensure_protocol(challenge, X402_PROTOCOL)?;
        let raw_wire = challenge
            .payload
            .get("payment_required")
            .ok_or_else(|| Error::PaymentProtocol("stored x402 challenge is missing".to_owned()))?;
        let stored_selected = challenge
            .payload
            .get("selected")
            .ok_or_else(|| Error::PaymentProtocol("x402 selected requirement is missing".to_owned()))?;
        let integrity = self.decode_challenge_wire(&challenge.resource, raw_wire, Some(stored_selected))?;
        let expected_hash =
            X402Wire::challenge_hash(&challenge.resource, challenge.expires_at, raw_wire, &challenge.nonce)?;
        let outer_recipient = canonicalize_payment_recipient(&challenge.asset.network, &challenge.recipient)
            .map_err(|_| Error::PaymentProtocol("x402 outer recipient is invalid".to_owned()))?;

        if challenge.asset != integrity.asset
            || challenge.amount != integrity.amount
            || challenge.recipient != outer_recipient
            || challenge.recipient != integrity.recipient
            || challenge.challenge_hash != expected_hash
            || challenge.expires_at <= 0
        {
            return Err(Error::PaymentProtocol(
                "stored x402 challenge integrity check failed".to_owned(),
            ));
        }
        Ok(integrity)
    }
}

pub(super) struct X402ChallengeIntegrity {
    pub(super) payment_required: X402PaymentRequired,
    pub(super) selected: X402Requirement,
    pub(super) selected_wire: Value,
    pub(super) asset: AssetIdentity,
    pub(super) amount: Amount,
    pub(super) recipient: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum X402Requirement {
    Evm(EvmPaymentRequirements),
    Solana(SolanaPaymentRequirements),
}

impl X402Requirement {
    pub(super) fn network(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.network.to_string(),
            Self::Solana(requirement) => requirement.network.to_string(),
        }
    }

    pub(super) fn asset(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.asset.to_string(),
            Self::Solana(requirement) => requirement.asset.to_string(),
        }
    }

    pub(super) fn pay_to(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.pay_to.to_string(),
            Self::Solana(requirement) => requirement.pay_to.to_string(),
        }
    }

    pub(super) fn max_timeout_seconds(&self) -> u64 {
        match self {
            Self::Evm(requirement) => requirement.max_timeout_seconds,
            Self::Solana(requirement) => requirement.max_timeout_seconds,
        }
    }

    pub(super) fn amount(&self) -> Result<Amount> {
        match self {
            Self::Evm(requirement) => X402Wire::amount_from_u256(requirement.amount.0),
            Self::Solana(requirement) => {
                let amount = Amount::new(u128::from(requirement.amount.inner()));
                if amount.is_zero() {
                    return Err(Error::PaymentProtocol(
                        "payment amount must be a positive base-unit integer".to_owned(),
                    ));
                }
                Ok(amount)
            }
        }
    }

    pub(super) fn to_value(&self) -> Result<Value> {
        match self {
            Self::Evm(requirement) => Ok(serde_json::to_value(requirement)?),
            Self::Solana(requirement) => Ok(serde_json::to_value(requirement)?),
        }
    }
}

pub(super) enum X402PaymentPayload {
    Evm(EvmPaymentPayload),
    Solana(SolanaPaymentPayload),
}

impl X402PaymentPayload {
    pub(super) fn payer(&self) -> Result<String> {
        match self {
            Self::Evm(payload) => Ok(X402Evm::authorization_payload(payload)?.authorization.from.to_string()),
            Self::Solana(payload) => Ok(X402Solana::payer(
                &X402Solana::decode_transaction(&payload.payload.transaction)?,
                &payload.accepted,
            )?
            .to_string()),
        }
    }
}

impl X402Wire {
    pub(super) fn validate_requirement(requirement: &X402Requirement, max_timeout: u64) -> Result<()> {
        let valid = match requirement {
            X402Requirement::Evm(requirement) => {
                requirement.scheme.as_ref() == EXACT_SCHEME
                    && X402Evm::valid_network(&requirement.network.to_string())
                    && matches!(
                        &requirement.extra,
                        AssetTransferMethod::Eip3009 { name, version } if name == "USDC" && version == "2"
                    )
                    && !requirement.amount.0.is_zero()
            }
            X402Requirement::Solana(requirement) => {
                requirement.scheme.as_ref() == EXACT_SCHEME
                    && X402Solana::valid_network(&requirement.network.to_string())
                    && requirement.amount.inner() > 0
            }
        };
        if !valid || requirement.max_timeout_seconds() == 0 || requirement.max_timeout_seconds() > max_timeout {
            return Err(Error::PaymentProtocol(
                "unsupported or unsafe x402 requirement".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn decode_payment_payload(
        value: &Value,
        integrity: &X402ChallengeIntegrity,
    ) -> Result<X402PaymentPayload> {
        match &integrity.selected {
            X402Requirement::Evm(_) => X402Evm::decode_payment_payload(value).map(X402PaymentPayload::Evm),
            X402Requirement::Solana(_) => X402Solana::decode_payment_payload(value).map(X402PaymentPayload::Solana),
        }
    }

    pub(super) fn validate_payload(
        challenge: &PaymentChallenge,
        integrity: &X402ChallengeIntegrity,
        payload: &X402PaymentPayload,
        now: Option<i64>,
    ) -> Result<()> {
        match (payload, &integrity.selected) {
            (X402PaymentPayload::Evm(payload), X402Requirement::Evm(requirement)) => {
                let authorization = &X402Evm::authorization_payload(payload)?.authorization;
                let valid_after = i64::try_from(authorization.valid_after.as_secs())
                    .map_err(|_| Error::PaymentProtocol("x402 validAfter is invalid".to_owned()))?;
                let valid_before = i64::try_from(authorization.valid_before.as_secs())
                    .map_err(|_| Error::PaymentProtocol("x402 validBefore is invalid".to_owned()))?;
                if !X402Wire::payload_resource_matches(
                    payload.resource.as_ref(),
                    integrity.payment_required.resource.as_ref(),
                    &challenge.resource,
                ) || payload.accepted != *requirement
                    || !payment_recipients_match(
                        &challenge.asset.network,
                        &authorization.to.to_string(),
                        &challenge.recipient,
                    )
                    || authorization.value != U256::from(challenge.amount.value())
                    || valid_after >= valid_before
                    || valid_before > challenge.expires_at
                    || !X402Evm::valid_signature(&X402Evm::authorization_payload(payload)?.signature)
                    || now.is_some_and(|now| now < valid_after || now >= valid_before)
                {
                    return Err(Error::PaymentProtocol(
                        "x402 EIP-3009 payload binding failed".to_owned(),
                    ));
                }
            }
            (X402PaymentPayload::Solana(payload), X402Requirement::Solana(requirement)) => {
                let transaction = X402Solana::decode_transaction(&payload.payload.transaction)?;
                let payer = X402Solana::payer(&transaction, &payload.accepted)?;
                if !X402Wire::payload_resource_matches(
                    payload.resource.as_ref(),
                    integrity.payment_required.resource.as_ref(),
                    &challenge.resource,
                ) || payload.accepted != *requirement
                    || !payment_recipients_match(
                        &challenge.asset.network,
                        &payload.accepted.pay_to.to_string(),
                        &challenge.recipient,
                    )
                    || payload.accepted.amount.inner() != u64::try_from(challenge.amount.value()).unwrap_or(u64::MAX)
                    || payload.accepted.asset.to_string()
                        != challenge.asset.token_contract.as_deref().unwrap_or_default()
                    || payer == *payload.accepted.extra.fee_payer.pubkey()
                {
                    return Err(Error::PaymentProtocol("x402 Solana payload binding failed".to_owned()));
                }
            }
            _ => {
                return Err(Error::PaymentProtocol(
                    "x402 payload does not match the selected chain scheme".to_owned(),
                ))
            }
        }
        Ok(())
    }

    pub(super) fn payload_resource_matches(
        resource: Option<&ResourceInfo>,
        expected: Option<&ResourceInfo>,
        url: &str,
    ) -> bool {
        resource.is_none_or(|resource| X402Wire::resource_matches(resource, expected) && resource.url == url)
    }

    pub(super) fn resource_matches(resource: &ResourceInfo, expected: Option<&ResourceInfo>) -> bool {
        expected.is_some_and(|expected| {
            resource.url == expected.url
                && resource.description == expected.description
                && resource.mime_type == expected.mime_type
        })
    }

    pub(super) fn deterministic_nonce(resource: &str, expires_at: i64, raw_wire: &Value) -> Result<String> {
        let expires_at = expires_at.to_string();
        let canonical_wire = X402Wire::canonical_json(raw_wire);
        Ok(X402Wire::hash_values(&[
            b"agentvault.x402.challenge.nonce.v2",
            resource.as_bytes(),
            expires_at.as_bytes(),
            &serde_json::to_vec(&canonical_wire)?,
        ]))
    }

    pub(super) fn server_identity(resource: &str, expires_at: i64, raw_wire: &Value) -> Result<(String, String)> {
        let mut nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut nonce);
        let nonce = hex::encode(nonce);
        let challenge_hash = X402Wire::challenge_hash(resource, expires_at, raw_wire, &nonce)?;
        Ok((nonce, challenge_hash))
    }

    pub(super) fn challenge_hash(resource: &str, expires_at: i64, raw_wire: &Value, nonce: &str) -> Result<String> {
        if nonce.len() != 64
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::PaymentProtocol("x402 challenge nonce is invalid".to_owned()));
        }
        let expires_at = expires_at.to_string();
        let canonical_wire = X402Wire::canonical_json(raw_wire);
        Ok(X402Wire::hash_values(&[
            b"agentvault.x402.challenge.v2",
            resource.as_bytes(),
            expires_at.as_bytes(),
            &serde_json::to_vec(&canonical_wire)?,
            nonce.as_bytes(),
        ]))
    }

    /// Produces a stable semantic representation across JSON parsers and SQL
    /// `json`/`jsonb` round-trips. Arrays retain protocol order; object keys are
    /// sorted recursively so storage-specific map ordering cannot invalidate a
    /// legitimate challenge.
    pub(super) fn canonical_json(value: &Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(items.iter().map(X402Wire::canonical_json).collect()),
            Value::Object(fields) => {
                let mut keys = fields.keys().collect::<Vec<_>>();
                keys.sort_unstable();
                let mut canonical = serde_json::Map::new();
                for key in keys {
                    canonical.insert(key.clone(), X402Wire::canonical_json(&fields[key]));
                }
                Value::Object(canonical)
            }
            scalar => scalar.clone(),
        }
    }

    pub(super) fn ensure_protocol(challenge: &PaymentChallenge, expected: &str) -> Result<()> {
        if challenge.protocol == expected {
            Ok(())
        } else {
            Err(Error::PaymentProtocol("protocol adapter mismatch".to_owned()))
        }
    }

    pub(super) fn amount_from_u256(value: U256) -> Result<Amount> {
        let value = u128::try_from(value)
            .map_err(|_| Error::PaymentProtocol("payment amount exceeds supported precision".to_owned()))?;
        let amount = Amount::new(value);
        if amount.is_zero() {
            return Err(Error::PaymentProtocol(
                "payment amount must be a positive base-unit integer".to_owned(),
            ));
        }
        Ok(amount)
    }

    pub(super) fn exact_object_fields(value: &Value, required: &[&str], allowed: &[&str], name: &str) -> Result<()> {
        let object = value
            .as_object()
            .ok_or_else(|| Error::PaymentProtocol(format!("x402 {name} must be an object")))?;
        if required.iter().any(|field| !object.contains_key(*field))
            || object.keys().any(|field| !allowed.contains(&field.as_str()))
        {
            return Err(Error::PaymentProtocol(format!(
                "x402 {name} fields do not match the official wire"
            )));
        }
        Ok(())
    }

    pub(crate) fn parse_verify_response(response: &[u8]) -> Result<VerifyResponse> {
        let wire = X402Wire::strict_variant_wire(
            response,
            "isValid",
            &["isValid", "payer"],
            &["isValid", "payer"],
            &["isValid", "invalidReason"],
            &["isValid", "invalidReason", "payer"],
        )?;
        serde_json::from_value(wire)
            .map_err(|error| Error::Settlement(format!("invalid x402 /verify response: {error}")))
    }

    pub(crate) fn parse_settle_response(response: &[u8]) -> Result<SettleResponse> {
        let wire = X402Wire::strict_variant_wire(
            response,
            "success",
            &["success", "payer", "transaction", "network"],
            &["success", "payer", "transaction", "network"],
            &["success", "errorReason", "network"],
            &["success", "errorReason", "network"],
        )?;
        serde_json::from_value(wire)
            .map_err(|error| Error::Settlement(format!("invalid x402 /settle response: {error}")))
    }

    pub(super) fn strict_variant_wire(
        response: &[u8],
        discriminant: &str,
        success_required: &[&str],
        success_allowed: &[&str],
        error_required: &[&str],
        error_allowed: &[&str],
    ) -> Result<Value> {
        let value: Value = serde_json::from_slice(response)
            .map_err(|error| Error::Settlement(format!("invalid x402 facilitator response: {error}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| Error::Settlement("x402 facilitator response must be an object".to_owned()))?;
        let success = object
            .get(discriminant)
            .and_then(Value::as_bool)
            .ok_or_else(|| Error::Settlement(format!("x402 facilitator response has no boolean {discriminant}")))?;
        let (required, allowed) = if success {
            (success_required, success_allowed)
        } else {
            (error_required, error_allowed)
        };
        if required.iter().any(|field| !object.contains_key(*field))
            || object.keys().any(|field| !allowed.contains(&field.as_str()))
        {
            return Err(Error::Settlement(
                "x402 facilitator response fields do not match the official wire variant".to_owned(),
            ));
        }
        Ok(value)
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

    pub(super) fn hash_values(values: &[&[u8]]) -> String {
        let mut hash = Sha256::new();
        for value in values {
            hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            hash.update(value);
        }
        hex::encode(hash.finalize())
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/x402_wire_test.rs"]
mod tests;
