use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{payment_recipients_match, Amount, AssetIdentity, PaymentCredential, PaymentIntent, PaymentReceipt};
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementOperation {
    Verify,
    Settle,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementProviderRequest {
    pub protocol: String,
    pub run_id: String,
    pub payment_payload: Value,
    pub payment_requirements: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettlementProviderResult {
    #[serde(default)]
    pub valid: bool,
    #[serde(default)]
    pub settled: bool,
    #[serde(default)]
    pub payer: Option<String>,
    #[serde(default)]
    pub transaction: Option<String>,
    #[serde(default)]
    pub settlement_ref: Option<String>,
    #[serde(default)]
    pub effect_ref: Option<String>,
    #[serde(default)]
    pub invalid_reason: Option<String>,
    #[serde(default)]
    pub asset: Option<AssetIdentity>,
    #[serde(default)]
    pub amount: Option<Amount>,
    #[serde(default)]
    pub recipient: Option<String>,
    #[serde(default)]
    pub confirmations: Option<u64>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

impl SettlementProviderResult {
    pub(crate) fn validate_confirmed_for(&self, intent: &PaymentIntent, minimum_confirmations: u64) -> Result<()> {
        let asset_matches = self
            .asset
            .as_ref()
            .is_some_and(|asset| asset_identity_matches(asset, &intent.asset));
        let recipient_matches = self
            .recipient
            .as_deref()
            .is_some_and(|recipient| payment_recipients_match(&intent.asset.network, recipient, &intent.recipient));
        let confirmations_match =
            minimum_confirmations == 0 || self.confirmations.is_some_and(|value| value >= minimum_confirmations);
        if !self.valid
            || !self.settled
            || !asset_matches
            || self.amount != Some(intent.amount)
            || !recipient_matches
            || !confirmations_match
            || (self.transaction.as_deref().is_none_or(str::is_empty)
                && self.settlement_ref.as_deref().is_none_or(str::is_empty))
        {
            return Err(Error::Settlement(
                "provider settlement evidence does not match the payment intent".to_owned(),
            ));
        }
        Ok(())
    }
}

fn asset_identity_matches(observed: &AssetIdentity, expected: &AssetIdentity) -> bool {
    observed.network == expected.network
        && observed.asset.eq_ignore_ascii_case(&expected.asset)
        && observed.decimals == expected.decimals
        && match (&observed.token_contract, &expected.token_contract) {
            (Some(observed), Some(expected)) => observed.eq_ignore_ascii_case(expected),
            (None, None) => true,
            _ => false,
        }
}

#[derive(Clone, Debug)]
pub struct IssuePaymentChallenge {
    pub scenario_id: String,
    pub run_id: String,
    pub price_id: String,
    pub resource: String,
    pub origin: String,
    pub protocol: String,
    pub request_hash: String,
}

#[derive(Clone, Debug)]
pub struct VerifyIncomingPayment {
    pub scenario_id: String,
    pub run_id: String,
    pub challenge_hash: String,
    pub request_hash: String,
    pub credential: PaymentCredential,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingVerification {
    pub intent: PaymentIntent,
    pub valid: bool,
    pub verification_ref: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SettleIncomingPayment {
    pub scenario_id: String,
    pub run_id: String,
    pub intent_id: String,
    pub verification_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettledIncomingPayment {
    pub intent: PaymentIntent,
    pub receipt: PaymentReceipt,
}

#[cfg(test)]
#[path = "../../tests/model/settlement_test.rs"]
mod tests;
