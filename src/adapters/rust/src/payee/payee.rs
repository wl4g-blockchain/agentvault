use crate::{
    percent_encode, ApiClient, CreateChallengeRequest, Error, OperationContext, PayeeConfig, PayeeOperation,
    PaymentChallenge, PaymentCredential, PaymentOffer, PaymentReceipt, PaymentVerification, ProtectionOutcome,
    ResolveOfferRequest, Result, SettlePaymentRequest, SettledIncomingPayment, VaultTransport, VerifyPaymentRequest,
};

/// Transport-neutral payee workflow. HTTP framework adapters construct a
/// [`PayeeOperation`], while this type owns challenge, verification, and
/// settlement sequencing.
pub struct PayeeMiddleware<T> {
    api: ApiClient<T>,
    config: PayeeConfig,
}

impl<T> Clone for PayeeMiddleware<T> {
    fn clone(&self) -> Self {
        Self {
            api: self.api.clone(),
            config: self.config.clone(),
        }
    }
}

impl<T> PayeeMiddleware<T> {
    pub fn new(transport: T, config: PayeeConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            api: ApiClient::new(transport),
            config,
        })
    }

    pub(crate) fn from_api(api: ApiClient<T>, config: PayeeConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { api, config })
    }

    #[must_use]
    pub const fn config(&self) -> &PayeeConfig {
        &self.config
    }

    #[must_use]
    pub fn axum(self, routes: Vec<crate::AxumPayeeRoute>) -> crate::AxumPayeeMiddleware<T> {
        let limit = self.config.max_body_bytes;
        crate::AxumPayeeMiddleware::new(self, routes).with_max_body_bytes(limit)
    }

    #[must_use]
    pub fn standard(self) -> crate::StandardPayeeMiddleware<T> {
        crate::StandardPayeeMiddleware::new(self)
    }
}

impl<T: VaultTransport> PayeeMiddleware<T> {
    pub async fn challenge(&self, operation: &PayeeOperation) -> Result<PaymentChallenge> {
        operation.validate()?;
        let context = OperationContext::new(operation.scenario_id.clone(), operation.run_id.clone())?;
        let challenge = self
            .create_challenge(&CreateChallengeRequest {
                context,
                offer_id: operation.offer_id.clone(),
                resource: operation.resource.clone(),
                origin: operation.origin.clone(),
                protocol: operation.protocol.clone(),
                request_hash: operation.request_hash.clone(),
            })
            .await?;
        validate_challenge_binding(operation, &challenge)?;
        Ok(challenge)
    }

    pub async fn protect(&self, operation: &PayeeOperation) -> Result<ProtectionOutcome> {
        tracing::info!(
            event.name = "agentvault.sdk.payee.intercepted",
            run.id = %operation.run_id,
            scenario.id = %operation.scenario_id
        );
        let challenge = self.challenge(operation).await?;
        tracing::info!(
            event.name = "agentvault.sdk.payee.challenge_created",
            run.id = %operation.run_id,
            payment.challenge_hash = %challenge.challenge_hash
        );
        let Some(payload) = operation.credential_payload.clone() else {
            return Ok(ProtectionOutcome::PaymentRequired {
                challenge: Box::new(challenge),
            });
        };
        let verify_context = OperationContext::new(operation.scenario_id.clone(), operation.run_id.clone())?;
        let challenge_hash = challenge.challenge_hash.clone();
        let verification = self
            .verify_payment(&VerifyPaymentRequest {
                context: verify_context,
                challenge_hash: challenge.challenge_hash.clone(),
                request_hash: operation.request_hash.clone(),
                credential: PaymentCredential {
                    protocol: operation.protocol.clone(),
                    challenge_hash: challenge_hash.clone(),
                    credential_hash: "untrusted".to_owned(),
                    payload,
                    http: None,
                },
            })
            .await?;
        if !verification.valid {
            return Err(Error::Api {
                status: 402,
                code: "PAYMENT_INVALID".to_owned(),
                message: "AgentVault rejected the presented payment".to_owned(),
            });
        }
        let verification_ref = verification
            .verification_ref
            .clone()
            .ok_or_else(|| Error::Decode("valid AgentVault verification omitted verification_ref".to_owned()))?;
        validate_verification_binding(operation, &challenge, &verification)?;
        tracing::info!(
            event.name = "agentvault.sdk.payee.verified",
            run.id = %operation.run_id,
            payment.intent_id = %verification.intent.id
        );
        let settle_context = OperationContext::new(operation.scenario_id.clone(), operation.run_id.clone())?;
        let payment = self
            .settle_payment(&SettlePaymentRequest {
                context: settle_context,
                intent_id: verification.intent.id.clone(),
                verification_ref,
            })
            .await?;
        validate_settlement_binding(operation, &verification, &payment)?;
        tracing::info!(
            event.name = "agentvault.sdk.payee.settled",
            run.id = %operation.run_id,
            payment.intent_id = %payment.intent.id
        );
        Ok(ProtectionOutcome::Authorized {
            payment: Box::new(payment),
        })
    }

    pub async fn resolve_offer(&self, request: &ResolveOfferRequest) -> Result<PaymentOffer> {
        crate::model_base::validate_required("scenario_id", &request.scenario_id)?;
        crate::model_base::validate_required("resource", &request.resource)?;
        self.api.post(crate::paths::RESOLVE_OFFER.to_owned(), request).await
    }

    async fn create_challenge(&self, request: &CreateChallengeRequest) -> Result<PaymentChallenge> {
        request.context.validate()?;
        crate::model_base::validate_required("offer_id", &request.offer_id)?;
        crate::model_base::validate_required("resource", &request.resource)?;
        crate::model_base::validate_required("origin", &request.origin)?;
        crate::model_base::validate_required("protocol", &request.protocol)?;
        crate::model_base::validate_required("request_hash", &request.request_hash)?;
        self.api.post(crate::paths::CREATE_CHALLENGE.to_owned(), request).await
    }

    async fn verify_payment(&self, request: &VerifyPaymentRequest) -> Result<PaymentVerification> {
        request.context.validate()?;
        crate::model_base::validate_required("challenge_hash", &request.challenge_hash)?;
        crate::model_base::validate_required("request_hash", &request.request_hash)?;
        crate::model_base::validate_required("credential.protocol", &request.credential.protocol)?;
        crate::model_base::validate_required("credential.challenge_hash", &request.credential.challenge_hash)?;
        crate::model_base::validate_required("credential.credential_hash", &request.credential.credential_hash)?;
        if request.challenge_hash != request.credential.challenge_hash {
            return Err(Error::InvalidArgument(
                "credential challenge_hash does not match the request".to_owned(),
            ));
        }
        self.api.post(crate::paths::VERIFY_PAYMENT.to_owned(), request).await
    }

    async fn settle_payment(&self, request: &SettlePaymentRequest) -> Result<SettledIncomingPayment> {
        request.context.validate()?;
        crate::model_base::validate_required("intent_id", &request.intent_id)?;
        crate::model_base::validate_required("verification_ref", &request.verification_ref)?;
        self.api.post(crate::paths::SETTLE_PAYMENT.to_owned(), request).await
    }

    pub async fn get_receipt(&self, scenario_id: &str, intent_id: &str) -> Result<PaymentReceipt> {
        crate::model_base::validate_required("scenario_id", scenario_id)?;
        crate::model_base::validate_required("intent_id", intent_id)?;
        self.api
            .get(format!(
                "{}/{intent_id}/receipt?scenario_id={scenario_id}",
                crate::paths::PAYMENTS,
                intent_id = percent_encode(intent_id),
                scenario_id = percent_encode(scenario_id)
            ))
            .await
    }
}

fn validate_challenge_binding(operation: &PayeeOperation, challenge: &PaymentChallenge) -> Result<()> {
    if challenge.scenario_id != operation.scenario_id
        || challenge.offer_id.as_deref() != Some(operation.offer_id.as_str())
        || challenge.resource != operation.resource
        || challenge.protocol != operation.protocol
    {
        return Err(Error::Decode(
            "AgentVault challenge is not bound to the protected operation".to_owned(),
        ));
    }
    for (name, value) in [
        ("challenge.asset.network", challenge.asset.network.as_str()),
        ("challenge.asset.asset", challenge.asset.asset.as_str()),
        ("challenge.amount", challenge.amount.as_str()),
        ("challenge.recipient", challenge.recipient.as_str()),
        ("challenge.nonce", challenge.nonce.as_str()),
        ("challenge.challenge_hash", challenge.challenge_hash.as_str()),
    ] {
        crate::model_base::validate_required(name, value)?;
    }
    Ok(())
}

fn validate_verification_binding(
    operation: &PayeeOperation,
    challenge: &PaymentChallenge,
    verification: &PaymentVerification,
) -> Result<()> {
    let intent = &verification.intent;
    if intent.id.is_empty()
        || intent.direction != crate::PaymentDirection::Inbound
        || intent.scenario_id != operation.scenario_id
        || intent.offer_id.as_deref() != Some(operation.offer_id.as_str())
        || intent.resource != operation.resource
        || intent.protocol != operation.protocol
        || intent.challenge_hash.as_deref() != Some(challenge.challenge_hash.as_str())
        || intent.request_hash != operation.request_hash
        || intent.asset != challenge.asset
        || intent.amount != challenge.amount
        || intent.recipient != challenge.recipient
    {
        return Err(Error::Decode(
            "AgentVault verification is not bound to the protected operation".to_owned(),
        ));
    }
    Ok(())
}

fn validate_settlement_binding(
    operation: &PayeeOperation,
    verification: &PaymentVerification,
    payment: &SettledIncomingPayment,
) -> Result<()> {
    let verified = &verification.intent;
    if payment.intent.id != verified.id
        || payment.intent.direction != crate::PaymentDirection::Inbound
        || payment.intent.scenario_id != verified.scenario_id
        || payment.intent.offer_id != verified.offer_id
        || payment.intent.resource != verified.resource
        || payment.intent.protocol != verified.protocol
        || payment.intent.challenge_hash != verified.challenge_hash
        || payment.intent.request_hash != verified.request_hash
        || payment.intent.asset != verified.asset
        || payment.intent.amount != verified.amount
        || payment.intent.recipient != verified.recipient
        || payment.intent.status != crate::PaymentStatus::Settled
        || payment.receipt.intent_id != payment.intent.id
        || payment.receipt.scenario_id != operation.scenario_id
        || payment.receipt.direction != crate::PaymentDirection::Inbound
        || payment.receipt.protocol != operation.protocol
        || payment.receipt.asset != payment.intent.asset
        || payment.receipt.recipient != payment.intent.recipient
        || payment.receipt.requested_amount != payment.intent.amount
        || payment.receipt.settled_amount != payment.intent.settled_amount
        || payment.receipt.status != crate::PaymentStatus::Settled
    {
        return Err(Error::Decode("invalid AgentVault settlement response".to_owned()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/payee/payee_test.rs"]
mod tests;
