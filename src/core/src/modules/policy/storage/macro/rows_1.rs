macro_rules! policy_rows_1 {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        impl EncodedPolicy {
            fn new(policy: &PaymentPolicy) -> Result<Self> {
                Ok(Self {
                    protocols: BaseRepository::encode_json(&policy.protocols, "pay_policy.protocols")?,
                    networks: BaseRepository::encode_json(&policy.networks, "pay_policy.networks")?,
                    assets: BaseRepository::encode_json(&policy.assets, "pay_policy.assets")?,
                    allowed_recipients: BaseRepository::encode_json(
                        &policy.allowed_recipients,
                        "pay_policy.allowed_recipients",
                    )?,
                    allowed_origins: BaseRepository::encode_json(
                        &policy.allowed_origins,
                        "pay_policy.allowed_origins",
                    )?,
                    inbound: EncodedLimits::new(policy.inbound_limits.as_ref())?,
                    outbound: EncodedLimits::new(policy.outbound_limits.as_ref())?,
                })
            }
        }
    };
}
pub(crate) use policy_rows_1;
