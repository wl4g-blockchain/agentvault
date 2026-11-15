macro_rules! policy_rows {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        impl EncodedLimits {
            fn new(limits: Option<&PaymentPolicyLimits>) -> Result<Self> {
                limits.map_or_else(
                    || {
                        Ok(Self {
                            max_per_payment: None,
                            max_per_day: None,
                            approval_threshold: None,
                            approval_enabled: true,
                            approval_overage: None,
                        })
                    },
                    |limits| {
                        Ok(Self {
                            max_per_payment: Some(AmountCodec::encode_amount(limits.max_per_payment)?),
                            max_per_day: Some(AmountCodec::encode_amount(limits.max_per_day)?),
                            approval_threshold: AmountCodec::encode_optional_amount(limits.approval_threshold)?,
                            approval_enabled: limits.approval_enabled,
                            approval_overage: AmountCodec::encode_optional_amount(limits.approval_overage)?,
                        })
                    },
                )
            }
        }
    };
}
pub(crate) use policy_rows;
