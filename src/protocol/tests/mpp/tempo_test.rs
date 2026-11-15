use super::*;
use agentvault_payee::protocol::{ProtocolSettlement,PaymentProtocol, mpp::{MppAdapter,MppAsset}};
const LIVE_TEMPO_RPC_URL: &str = "http://127.0.0.1:28546";
const LIVE_TEMPO_CHAIN_ID: u64 = 1_337;
const LIVE_TEMPO_PATH_USD: &str = "0x20c0000000000000000000000000000000000000";
const LIVE_TEMPO_DEV_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LiveTempoSettlementRequest {
    protocol: String,
    run_id: String,
    payment_payload: Value,
    payment_requirements: Value,
}

#[derive(Clone)]
struct LiveTempoFacilitator {
    method: mpp::server::TempoChargeMethod<mpp::server::TempoProvider>,
    provider: mpp::server::TempoProvider,
    asset: AssetIdentity,
    drop_settle_response: std::sync::Arc<std::sync::atomic::AtomicBool>,
    broadcasts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

struct ValidatedLiveCharge {
    credential: mpp::PaymentCredential,
    charge: mpp::ChargeRequest,
    transaction_hash: B256,
    payer: String,
    amount: Amount,
    recipient: String,
}

impl LiveTempoFacilitator {
    async fn execute(
        &self,
        request: &LiveTempoSettlementRequest,
        settle: bool,
    ) -> crate::storage::reconciliation::SettlementProviderResult {
        match self.execute_checked(request, settle).await {
            Ok(result) => result,
            Err(reason) => crate::storage::reconciliation::SettlementProviderResult {
                valid: false,
                settled: false,
                payer: None,
                transaction: None,
                settlement_ref: None,
                effect_ref: None,
                invalid_reason: Some(reason),
                asset: None,
                amount: None,
                recipient: None,
                confirmations: None,
                extensions: BTreeMap::new(),
            },
        }
    }

    async fn execute_checked(
        &self,
        request: &LiveTempoSettlementRequest,
        settle: bool,
    ) -> std::result::Result<crate::storage::reconciliation::SettlementProviderResult, String> {
        use alloy::{network::ReceiptResponse as _, providers::Provider as _};
        use mpp::ChargeMethod as _;

        let validated = self.validate_charge(request).await?;
        let expected_hash = validated.transaction_hash;
        let mut receipt = self
            .provider
            .get_transaction_receipt(expected_hash)
            .await
            .map_err(|error| format!("query Tempo receipt: {error}"))?;

        if settle && receipt.is_none() {
            let protocol_receipt = self
                .method
                .broadcast(&validated.credential, &validated.charge)
                .await
                .map_err(|error| error.to_string())?;
            let broadcast_hash = protocol_receipt
                .reference
                .parse::<B256>()
                .map_err(|error| format!("invalid Tempo receipt hash: {error}"))?;
            if broadcast_hash != expected_hash {
                return Err("facilitator broadcast returned a different transaction".to_owned());
            }
            self.broadcasts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            for _ in 0..40 {
                receipt = self
                    .provider
                    .get_transaction_receipt(expected_hash)
                    .await
                    .map_err(|error| format!("query broadcast Tempo receipt: {error}"))?;
                if receipt.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        let mut extensions = BTreeMap::new();
        let (settled, confirmations) = if let Some(receipt) = receipt {
            if !receipt.status() || format!("{:#x}", receipt.from()) != validated.payer {
                return Err("Tempo receipt status or payer is invalid".to_owned());
            }
            let (gas_limit, fee) = live_tempo_gas_evidence(
                &self.provider,
                expected_hash,
                receipt.gas_used(),
                receipt.effective_gas_price(),
            )
            .await?;
            let gas_cost_attodollars = U256::from(receipt.gas_used()) * U256::from(receipt.effective_gas_price());
            extensions.insert("gas_limit".to_owned(), json!(gas_limit));
            extensions.insert("gas_used".to_owned(), json!(receipt.gas_used()));
            extensions.insert(
                "effective_gas_price".to_owned(),
                json!(receipt.effective_gas_price().to_string()),
            );
            extensions.insert("gas_fee".to_owned(), json!(fee.to_string()));
            extensions.insert(
                "gas_cost_attodollars".to_owned(),
                json!(gas_cost_attodollars.to_string()),
            );
            extensions.insert("fee_payer".to_owned(), json!(format!("{:#x}", receipt.fee_payer)));
            extensions.insert(
                "fee_token".to_owned(),
                json!(receipt.fee_token.map(|token| format!("{token:#x}"))),
            );
            let current_block = self
                .provider
                .get_block_number()
                .await
                .map_err(|error| format!("query Tempo block number: {error}"))?;
            let receipt_block = receipt
                .block_number()
                .ok_or_else(|| "confirmed Tempo receipt omitted its block".to_owned())?;
            (
                true,
                Some(current_block.saturating_sub(receipt_block).saturating_add(1)),
            )
        } else {
            (false, None)
        };

        Ok(crate::storage::reconciliation::SettlementProviderResult {
            valid: true,
            settled,
            payer: Some(validated.payer),
            transaction: settled.then(|| format!("{expected_hash:#x}")),
            settlement_ref: None,
            effect_ref: settled.then(|| "tempo:tip20-transfer:0".to_owned()),
            invalid_reason: None,
            asset: Some(self.asset.clone()),
            amount: Some(validated.amount),
            recipient: Some(validated.recipient),
            confirmations,
            extensions,
        })
    }

    async fn validate_charge(
        &self,
        request: &LiveTempoSettlementRequest,
    ) -> std::result::Result<ValidatedLiveCharge, String> {
        use mpp::ChargeMethod as _;

        if request.protocol != MPP_PROTOCOL || request.run_id.trim().is_empty() {
            return Err("facilitator request identity is invalid".to_owned());
        }
        let challenge_header = request
            .payment_requirements
            .get("www_authenticate")
            .and_then(Value::as_str)
            .ok_or_else(|| "facilitator request omitted the MPP challenge".to_owned())?;
        let challenge = mpp::parse_www_authenticate(challenge_header).map_err(|error| error.to_string())?;
        let authorization = request
            .payment_payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| "facilitator request omitted the MPP credential".to_owned())?;
        let credential = mpp::parse_authorization(authorization).map_err(|error| error.to_string())?;
        if serde_json::to_value(&credential.challenge).map_err(|error| error.to_string())?
            != serde_json::to_value(challenge.to_echo()).map_err(|error| error.to_string())?
        {
            return Err("facilitator challenge echo mismatch".to_owned());
        }
        let charge: mpp::ChargeRequest = challenge.request.decode().map_err(|error| error.to_string())?;
        let validation = self
            .method
            .validate(&credential, &charge)
            .await
            .map_err(|error| error.to_string())?;
        let payload = credential.charge_payload().map_err(|error| error.to_string())?;
        let transaction = payload
            .signed_tx()
            .ok_or_else(|| "facilitator accepts only Tempo transaction credentials".to_owned())?;
        let transaction_bytes = transaction
            .parse::<alloy_primitives::Bytes>()
            .map_err(|error| format!("invalid Tempo transaction bytes: {error}"))?;
        let payer = validation
            .source
            .as_deref()
            .and_then(|source| source.rsplit(':').next())
            .ok_or_else(|| "validated Tempo credential omitted the payer".to_owned())?
            .to_ascii_lowercase();
        let amount = charge
            .amount
            .parse::<u128>()
            .map(Amount::new)
            .map_err(|error| format!("invalid validated Tempo amount: {error}"))?;
        let recipient = charge
            .recipient
            .as_deref()
            .ok_or_else(|| "validated Tempo charge omitted the recipient".to_owned())?
            .to_ascii_lowercase();
        Ok(ValidatedLiveCharge {
            credential,
            charge,
            transaction_hash: alloy_primitives::keccak256(&transaction_bytes),
            payer,
            amount,
            recipient,
        })
    }
}

async fn live_tempo_verify(
    axum::extract::State(facilitator): axum::extract::State<LiveTempoFacilitator>,
    axum::Json(request): axum::Json<LiveTempoSettlementRequest>,
) -> axum::Json<crate::storage::reconciliation::SettlementProviderResult> {
    let result = facilitator.execute(&request, false).await;
    println!(
        "{}",
        json!({"event": "agentvault.e2e.mpp.verify", "run_id": request.run_id,
            "valid": result.valid, "settled": result.settled,
            "invalid_reason": result.invalid_reason})
    );
    axum::Json(result)
}

async fn live_tempo_settle(
    axum::extract::State(facilitator): axum::extract::State<LiveTempoFacilitator>,
    axum::Json(request): axum::Json<LiveTempoSettlementRequest>,
) -> (
    axum::http::StatusCode,
    axum::Json<crate::storage::reconciliation::SettlementProviderResult>,
) {
    let result = facilitator.execute(&request, true).await;
    println!(
        "{}",
        json!({"event": "agentvault.e2e.mpp.settle", "run_id": request.run_id,
            "valid": result.valid, "settled": result.settled,
            "transaction": result.transaction})
    );
    if result.settled
        && facilitator
            .drop_settle_response
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(result));
    }
    (axum::http::StatusCode::OK, axum::Json(result))
}

async fn live_tempo_balance(provider: &mpp::server::TempoProvider, token: Address, account: Address) -> U256 {
    use alloy::providers::Provider as _;

    let input = ITIP20::balanceOfCall::new((account,)).abi_encode();
    let result = provider
        .call(
            alloy::rpc::types::TransactionRequest::default()
                .to(token)
                .input(alloy::rpc::types::TransactionInput::new(Bytes::from(input)))
                .into(),
        )
        .await
        .expect("Tempo balanceOf call failed");
    U256::from_be_slice(&result)
}

async fn fund_live_tempo_payer(provider: &mpp::server::TempoProvider, token: Address, payer: Address) {
    use alloy::{network::ReceiptResponse as _, providers::Provider as _};

    let dev_key = hex::decode(LIVE_TEMPO_DEV_PRIVATE_KEY).expect("dev key hex");
    let dev = crate::model::wallet::WalletCrypto::address(&dev_key)
        .expect("derive Tempo dev account")
        .parse::<Address>()
        .expect("Tempo dev account address");
    let chain_id = provider.get_chain_id().await.expect("query Tempo chain ID");
    let nonce = provider
        .get_transaction_count(dev)
        .await
        .expect("query Tempo dev nonce");
    let gas_price = provider.get_gas_price().await.expect("query Tempo gas price");
    let funding_amount = U256::from(10_000_000_000_u64);
    let transaction = TempoTransaction {
        chain_id,
        nonce,
        gas_limit: 5_000_000,
        max_fee_per_gas: gas_price,
        max_priority_fee_per_gas: gas_price,
        calls: vec![Call {
            to: TxKind::Call(token),
            value: U256::ZERO,
            input: Bytes::from(ITIP20::transferCall::new((payer, funding_amount)).abi_encode()),
        }],
        ..TempoTransaction::default()
    };
    let signature =
        crate::model::wallet::WalletCrypto::sign_eip712_digest(&dev_key, transaction.signature_hash().as_slice())
            .expect("sign Tempo payer funding transaction");
    let signature = Signature::from_raw(&signature).expect("decode Tempo payer funding signature");
    let raw = format!(
        "0x{}",
        hex::encode(transaction.into_signed(signature.into()).encoded_2718())
    );
    let transaction_hash: B256 = provider
        .raw_request("eth_sendRawTransaction".into(), (raw,))
        .await
        .expect("broadcast Tempo payer funding transaction");
    for _ in 0..40 {
        if let Some(receipt) = provider
            .get_transaction_receipt(transaction_hash)
            .await
            .expect("query Tempo payer funding receipt")
        {
            assert!(receipt.status(), "Tempo payer funding reverted");
            assert_eq!(live_tempo_balance(provider, token, payer).await, funding_amount);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("Tempo payer funding was not confirmed");
}

struct LiveTempoAccounts {
    token: Address,
    payer: Address,
    recipient: Address,
}

struct PreparedLiveTempoPayment {
    accounts: LiveTempoAccounts,
    recipient: String,
    asset: AssetIdentity,
    amount: Amount,
    payer_protocol: MppAdapter,
    settlement: ProtocolSettlement,
}

async fn prepare_live_tempo_payment(provider: &mpp::server::TempoProvider) -> PreparedLiveTempoPayment {
    let token = LIVE_TEMPO_PATH_USD.parse::<Address>().expect("pathUSD address");
    let payer_key = crate::model::wallet::WalletCrypto::generate_private_key();
    let payer = crate::model::wallet::WalletCrypto::address(&payer_key).expect("derive isolated payer");
    let payer_address = payer.parse::<Address>().expect("payer address");
    fund_live_tempo_payer(provider, token, payer_address).await;
    let recipient_key = crate::model::wallet::WalletCrypto::generate_private_key();
    let recipient = crate::model::wallet::WalletCrypto::address(&recipient_key).expect("derive recipient");
    let recipient_address = recipient.parse::<Address>().expect("recipient address");
    let asset = AssetIdentity {
        network: format!("eip155:{LIVE_TEMPO_CHAIN_ID}"),
        asset: "pathUSD".to_owned(),
        token_contract: Some(LIVE_TEMPO_PATH_USD.to_owned()),
        decimals: 6,
    };
    let configured_asset = MppAsset {
        network: asset.network.clone(),
        currency: LIVE_TEMPO_PATH_USD.to_owned(),
        symbol: asset.asset.clone(),
        token_contract: asset.token_contract.clone(),
        decimals: asset.decimals,
    };
    let issuer_adapter = MppAdapter::new_payee(
        vec![configured_asset.clone()],
        "marketpulse.local",
        "tempo-e2e-challenge-secret-32-bytes",
        60,
    )
    .expect("create MPP payee adapter");
    let payer_protocol = MppAdapter::new_payer(vec![configured_asset], 60).expect("create MPP payer adapter");
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let price = PaymentPrice {
        id: "price-mpp-tempo-live".to_owned(),
        scenario_id: "scenario-mpp-tempo-live".to_owned(),
        policy_id: "policy-mpp-tempo-live".to_owned(),
        resource: "https://marketpulse.local/mcp/realtime".to_owned(),
        protocols: vec![MPP_PROTOCOL.to_owned()],
        asset: asset.clone(),
        amount: Amount::new(1_000_000),
        wallet_id: "wallet-mpp-tempo-payee".to_owned(),
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let issued = issuer_adapter
        .build_challenge(&price, &recipient, now)
        .await
        .expect("issue MPP charge challenge");
    let challenge = payer_protocol
        .parse_challenge(
            "scenario-mpp-tempo-live",
            &price.resource,
            &issued.payload,
            now,
            Some(issued.expires_at),
        )
        .await
        .expect("payer parses MPP response header");
    let signing = payer_protocol
        .prepare_signing(&challenge, &payer, now + 1)
        .await
        .expect("Vault prepares intent-bound Tempo signing plan");
    let signature = crate::model::wallet::WalletCrypto::sign_eip712_digest(&payer_key, &signing.digest)
        .expect("Vault signs the Tempo transaction digest");
    let credential = payer_protocol
        .complete_signing(&challenge, signing, &signature)
        .await
        .expect("Vault creates the official MPP credential");
    let validation = issuer_adapter
        .verify_credential(&issued, &credential, now + 2)
        .await
        .expect("payee validates the signed credential binding");
    assert_eq!(validation.payer.as_deref(), Some(payer.as_str()));
    assert!(validation.requires_external_settlement);
    let settlement = issuer_adapter
        .settle_payment(&issued, &credential)
        .await
        .expect("payee maps the credential to facilitator DTOs");
    PreparedLiveTempoPayment {
        accounts: LiveTempoAccounts {
            token,
            payer: payer_address,
            recipient: recipient_address,
        },
        recipient,
        asset,
        amount: price.amount,
        payer_protocol,
        settlement,
    }
}

async fn start_live_tempo_facilitator(
    provider: &mpp::server::TempoProvider,
    asset: &AssetIdentity,
) -> (
    agentvault_payee::handler::provider::HttpSettlementProvider,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use axum::routing::post;

    let broadcasts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let facilitator = LiveTempoFacilitator {
        method: mpp::server::TempoChargeMethod::new(provider.clone())
            .with_store(std::sync::Arc::new(mpp::MemoryStore::new())),
        provider: provider.clone(),
        asset: asset.clone(),
        drop_settle_response: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        broadcasts: broadcasts.clone(),
    };
    let app = axum::Router::new()
        .route("/verify", post(live_tempo_verify))
        .route("/settle", post(live_tempo_settle))
        .with_state(facilitator);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind MPP facilitator");
    let base_url = format!("http://{}", listener.local_addr().expect("facilitator address"));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.expect("facilitator server") });
    let external = agentvault_payee::handler::provider::HttpSettlementProvider::new(
        &agentvault_payee::handler::provider::SettlementProviderConfig {
            base_url,
            allow_insecure_loopback: true,
            ..agentvault_payee::handler::provider::SettlementProviderConfig::default()
        },
        reqwest::header::HeaderMap::new(),
    )
    .expect("create production HTTP settlement provider");
    (external, broadcasts, server)
}

async fn live_tempo_gas_evidence(
    provider: &mpp::server::TempoProvider,
    transaction_hash: B256,
    gas_used: u64,
    effective_gas_price: u128,
) -> std::result::Result<(u64, U256), String> {
    use alloy::{consensus::Transaction as _, providers::Provider as _};

    let transaction = provider
        .get_transaction_by_hash(transaction_hash)
        .await
        .map_err(|error| format!("query confirmed Tempo transaction: {error}"))?
        .ok_or_else(|| "confirmed Tempo transaction is missing".to_owned())?;
    let gas_limit = transaction.gas_limit();
    if gas_used > gas_limit {
        return Err("Tempo receipt gas usage exceeds the signed transaction cap".to_owned());
    }
    let fee = tempo_primitives::transaction::calc_gas_balance_spending(gas_used, effective_gas_price);
    Ok((gas_limit, fee))
}

async fn assert_live_tempo_accounting(
    provider: &mpp::server::TempoProvider,
    accounts: &LiveTempoAccounts,
    transaction: &str,
    amount: Amount,
    payer_before: U256,
    recipient_before: U256,
) -> (U256, U256, U256, u64, u64) {
    use alloy::{network::ReceiptResponse as _, providers::Provider as _};

    let transaction_hash = transaction.parse::<B256>().expect("transaction hash");
    let receipt = provider
        .get_transaction_receipt(transaction_hash)
        .await
        .expect("query confirmed receipt")
        .expect("confirmed Tempo receipt");
    assert!(receipt.status());
    assert_eq!(receipt.from(), accounts.payer);
    assert_eq!(receipt.fee_payer, accounts.payer);
    assert_eq!(receipt.fee_token, Some(accounts.token));
    let payer_after = live_tempo_balance(provider, accounts.token, accounts.payer).await;
    let recipient_after = live_tempo_balance(provider, accounts.token, accounts.recipient).await;
    let amount = U256::from(amount.value());
    let gas_used = receipt.gas_used();
    let (gas_limit, gas_fee) =
        live_tempo_gas_evidence(provider, transaction_hash, gas_used, receipt.effective_gas_price())
            .await
            .expect("derive Tempo fee-token debit from confirmed transaction");
    assert_eq!(recipient_after - recipient_before, amount);
    assert_eq!(payer_before - payer_after, amount + gas_fee);
    assert_eq!(payer_before + recipient_before, payer_after + recipient_after + gas_fee);
    (payer_after, recipient_after, gas_fee, gas_limit, gas_used)
}

struct LiveTempoEvidence {
    transaction: String,
    amount: Amount,
    gas_fee: U256,
    payer_before: U256,
    payer_after: U256,
    recipient_before: U256,
    recipient_after: U256,
    broadcasts: usize,
    gas_limit: u64,
    gas_used: u64,
}

impl LiveTempoEvidence {
    fn validate_and_emit(&self, payer_protocol: &MppAdapter, run_id: &str) {
        let receipt_header = mpp::Receipt::success(TEMPO_METHOD, self.transaction.clone())
            .to_header()
            .expect("MPP receipt header");
        let merchant_evidence = payer_protocol
            .decode_merchant_evidence(&receipt_header)
            .expect("payer decodes MPP merchant evidence");
        assert!(merchant_evidence.accepted);
        assert_eq!(
            merchant_evidence.transaction.as_deref(),
            Some(self.transaction.as_str())
        );
        println!(
            "MPP_TEMPO_EVIDENCE={}",
            json!({
                "run_id": run_id,
                "protocol": MPP_PROTOCOL,
                "method": TEMPO_METHOD,
                "chain_id": LIVE_TEMPO_CHAIN_ID,
                "transaction": self.transaction,
                "amount": self.amount.to_string(),
                "gas_fee": self.gas_fee.to_string(),
                "gas_limit": self.gas_limit,
                "gas_used": self.gas_used,
                "payer_before": self.payer_before.to_string(),
                "payer_after": self.payer_after.to_string(),
                "recipient_before": self.recipient_before.to_string(),
                "recipient_after": self.recipient_after.to_string(),
                "broadcasts": self.broadcasts,
                "recovered_after_lost_response": true,
            })
        );
    }
}

#[tokio::test]
#[ignore = "requires the pinned Tempo localnet E2E profile"]
async fn live_tempo_charge_survives_lost_settle_response_and_conserves_token_with_gas() {
    use crate::storage::reconciliation::SettlementOperation;
    use crate::storage::reconciliation::SettlementProviderRequest;
    use agentvault_payee::handler::provider::ExternalSettlementProvider;
    use alloy::providers::Provider as _;

    let rpc_url = std::env::var("TEMPO_RPC_URL").unwrap_or_else(|_| LIVE_TEMPO_RPC_URL.to_owned());
    let provider = mpp::server::tempo_provider(&rpc_url).expect("create Tempo provider");
    assert_eq!(
        provider.get_chain_id().await.expect("query Tempo chain ID"),
        LIVE_TEMPO_CHAIN_ID,
        "the E2E endpoint is not the pinned Tempo localnet"
    );
    let PreparedLiveTempoPayment {
        accounts,
        recipient,
        asset,
        amount,
        payer_protocol,
        settlement,
    } = prepare_live_tempo_payment(&provider).await;
    let (external, broadcasts, server) = start_live_tempo_facilitator(&provider, &asset).await;
    let request = SettlementProviderRequest {
        protocol: settlement.protocol,
        run_id: "run-mpp-tempo-live".to_owned(),
        payment_payload: settlement.payment_payload,
        payment_requirements: settlement.payment_requirements,
    };

    let payer_before = live_tempo_balance(&provider, accounts.token, accounts.payer).await;
    let recipient_before = live_tempo_balance(&provider, accounts.token, accounts.recipient).await;
    let verified = external
        .execute(SettlementOperation::Verify, &request)
        .await
        .expect("offline facilitator verify");
    assert!(
        verified.valid && !verified.settled,
        "offline facilitator verification failed: {:?}",
        verified.invalid_reason
    );
    assert_eq!(
        (
            live_tempo_balance(&provider, accounts.token, accounts.payer).await,
            live_tempo_balance(&provider, accounts.token, accounts.recipient).await,
        ),
        (payer_before, recipient_before)
    );

    let lost_response = external.execute(SettlementOperation::Settle, &request).await;
    assert!(lost_response.is_err(), "the E2E must simulate a lost settle response");
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);

    let recovered = external
        .execute(SettlementOperation::Verify, &request)
        .await
        .expect("recover settlement through read-only verify");
    assert!(recovered.valid && recovered.settled);
    assert_eq!(recovered.amount, Some(amount));
    assert_eq!(recovered.asset.as_ref(), Some(&asset));
    assert_eq!(recovered.recipient.as_deref(), Some(recipient.as_str()));
    let transaction = recovered.transaction.expect("confirmed transaction");
    let (payer_after, recipient_after, gas_fee, gas_limit, gas_used) = assert_live_tempo_accounting(
        &provider,
        &accounts,
        &transaction,
        amount,
        payer_before,
        recipient_before,
    )
    .await;

    let duplicate = external
        .execute(SettlementOperation::Settle, &request)
        .await
        .expect("idempotent settlement retry");
    assert_eq!(duplicate.transaction.as_deref(), Some(transaction.as_str()));
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        (
            live_tempo_balance(&provider, accounts.token, accounts.payer).await,
            live_tempo_balance(&provider, accounts.token, accounts.recipient).await,
        ),
        (payer_after, recipient_after)
    );

    LiveTempoEvidence {
        transaction,
        amount,
        gas_fee,
        payer_before,
        payer_after,
        recipient_before,
        recipient_after,
        broadcasts: broadcasts.load(std::sync::atomic::Ordering::SeqCst),
        gas_limit,
        gas_used,
    }
    .validate_and_emit(&payer_protocol, &request.run_id);
    server.abort();
    let _ = server.await;
}


mod reconciliation { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mpp/reconciliation_test.rs")); }
