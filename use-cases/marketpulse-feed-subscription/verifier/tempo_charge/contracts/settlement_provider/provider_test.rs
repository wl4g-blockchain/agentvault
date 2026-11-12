use super::*;

struct ChargeFixture;

impl ChargeFixture {
    fn asset() -> AssetIdentity {
        AssetIdentity {
            network: "eip155:1337".to_owned(),
            asset: "pathUSD".to_owned(),
            token_contract: Some("0x20c0000000000000000000000000000000000000".to_owned()),
            decimals: 6,
        }
    }

    fn recipient() -> Address {
        "0x3333333333333333333333333333333333333333".parse().unwrap()
    }

    fn charge() -> mpp::ChargeRequest {
        serde_json::from_value(json!({
            "amount": "1000000", "currency": Self::asset().token_contract,
            "recipient": format!("{:#x}", Self::recipient()),
            "methodDetails": { "chainId": 1337 }
        }))
        .unwrap()
    }
}

#[test]
fn charge_scope_rejects_network_currency_recipient_and_unsupported_modes() {
    let asset = ChargeFixture::asset();
    let recipient = ChargeFixture::recipient();
    let charge = ChargeFixture::charge();
    assert!(TempoFacilitator::validate_charge_scope(&charge, &asset, recipient).is_ok());
    for mutation in [
        json!({"methodDetails":{"chainId":31337}}),
        json!({"methodDetails":{"chainId":"1337"}}),
        json!({"methodDetails":{"chainId":1337,"feePayer":true}}),
        json!({"methodDetails":{"chainId":1337,"machineTokenEnabled":true}}),
        json!({"currency":"0x1111111111111111111111111111111111111111"}),
        json!({"recipient":"0x2222222222222222222222222222222222222222"}),
        json!({"amount":"0"}),
    ] {
        let mut candidate = serde_json::to_value(&charge).unwrap();
        candidate
            .as_object_mut()
            .unwrap()
            .extend(mutation.as_object().unwrap().clone());
        let candidate = serde_json::from_value(candidate).unwrap();
        assert!(TempoFacilitator::validate_charge_scope(&candidate, &asset, recipient).is_err());
    }
}

#[test]
fn provider_wire_rejects_missing_or_unknown_fields_without_echoing_credential() {
    assert!(serde_json::from_value::<ProviderRequest>(json!({
        "protocol":"mpp", "runId":"run-1", "paymentPayload":{}, "paymentRequirements":{},
        "payment_response": "old transport"
    }))
    .is_err());
    for (protocol, run_id) in [("x402", "run-1"), ("mpp", ""), ("mpp", "bad\nrun")] {
        let request = ProviderRequest {
            protocol: protocol.to_owned(),
            run_id: run_id.to_owned(),
            payment_payload: json!({"authorization":"private signed transaction"}),
            payment_requirements: json!({}),
        };
        let reason = request.parse_mpp().err().unwrap();
        assert_eq!(reason, "provider request identity is invalid");
        assert!(!reason.contains("private"));
    }
}

#[test]
fn rejected_result_never_claims_a_chain_effect() {
    let result = TempoFacilitator::rejected("invalid signature".to_owned());
    assert!(!result.valid && !result.settled);
    assert!(result.transaction.is_none() && result.confirmations.is_none());
    assert!(result.asset.is_none() && result.amount.is_none());
}
