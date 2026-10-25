use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    model::{
        Amount, AssetIdentity, BudgetUnit, PaymentChallenge, PaymentClient, PaymentOffer, PaymentPolicy,
        PaymentPolicyLimits, PaymentReceipt, PaymentScenario, Wallet,
    },
    protocol::{PaymentProtocol, ProtocolSettlement, ProtocolSigningRequest, ProtocolValidation},
    storage::{EncryptedWalletKeyVersion, PaymentWallet, WalletKeyStatus, WalletPaymentSignature},
    utils::{
        aead::{new_uuid_nonce, WALLET_KEY_ENCRYPTION_SCHEME},
        secure_material::Aes256GcmMaterialProtector,
    },
};

use super::*;

const NOW: i64 = 100;
const PAYER: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RECIPIENT: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct CapturedCommit {
    guard: SigningGuard,
    events: Vec<PaymentEvent>,
    outbox: Vec<OutboxMessage>,
}

struct MockStorage {
    principal: String,
    state: Mutex<OutboundSigningRead>,
    commits: AtomicUsize,
    captured: Mutex<Option<CapturedCommit>>,
}

impl MockStorage {
    fn new(read: OutboundSigningRead) -> Self {
        Self {
            principal: "client-principal".to_owned(),
            state: Mutex::new(read),
            commits: AtomicUsize::new(0),
            captured: Mutex::new(None),
        }
    }
}

#[async_trait]
impl SigningStorage for MockStorage {
    async fn load_for_principal(
        &self,
        expected_principal: Option<&str>,
        intent_id: &str,
    ) -> Result<Option<OutboundSigningRead>> {
        if expected_principal.is_some_and(|principal| principal != self.principal) {
            return Err(Error::Forbidden);
        }
        let read = self.state.lock().unwrap().clone();
        let stored_id = match &read {
            OutboundSigningRead::Authorized(record) => &record.identity.intent.id,
            OutboundSigningRead::Signed(record) => &record.identity.intent.id,
        };
        Ok((stored_id == intent_id).then_some(read))
    }

    async fn commit_signed(&self, write: SigningWrite<'_>) -> Result<IdempotentWrite<PersistedSignedPayment>> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        let mut state = self.state.lock().unwrap();
        if let OutboundSigningRead::Signed(record) = &*state {
            return Ok(IdempotentWrite::Replay((**record).clone()));
        }
        let OutboundSigningRead::Authorized(current) = &*state else {
            unreachable!();
        };
        if write.guard.principal != self.principal
            || write.guard.expected_intent_updated_at != current.identity.intent.updated_at
            || write.guard.expected_wallet_updated_at != current.wallet.updated_at
            || write.guard.expected_key_version != current.active_key.version
            || write.challenge != &current.identity.challenge
            || write.intent.status != PaymentStatus::Signed
            || write.events.len() != 2
            || write.events[0].event_type != PaymentEventType::SignRequested
            || write.events[1].event_type != PaymentEventType::Signed
            || write.outbox.len() != 1
        {
            return Err(Error::Storage("mock signing CAS rejected stale write".to_owned()));
        }
        let mut identity = current.identity.clone();
        identity.intent = write.intent.clone();
        let persisted = PersistedSignedPayment {
            identity,
            credential_hash: write.credential_hash.to_owned(),
            sealed_credential: write.sealed_credential.clone(),
        };
        *self.captured.lock().unwrap() = Some(CapturedCommit {
            guard: write.guard.clone(),
            events: write.events.to_vec(),
            outbox: write.outbox.to_vec(),
        });
        *state = OutboundSigningRead::Signed(Box::new(persisted.clone()));
        Ok(IdempotentWrite::Inserted(persisted))
    }
}

struct MockBalances {
    snapshot: crate::handler::WalletBalanceSnapshot,
    calls: AtomicUsize,
}

#[async_trait]
impl WalletBalanceSource for MockBalances {
    async fn load(&self, wallet: &Wallet, asset: &AssetIdentity) -> Result<crate::handler::WalletBalanceSnapshot> {
        assert_eq!(wallet.id, "wallet-1");
        assert_eq!(asset.asset, "USDC");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.snapshot)
    }
}

struct MockSigner {
    calls: AtomicUsize,
}

#[async_trait]
impl WalletPaymentSigner for MockSigner {
    async fn sign_payment(&self, request: IntentBoundPaymentSignatureRequest<'_>) -> Result<WalletPaymentSignature> {
        assert_eq!(request.intent().id, "intent-1");
        assert_eq!(request.challenge().challenge_hash, "challenge-1");
        assert_eq!(request.wallet().wallet.id, "wallet-1");
        assert_eq!(request.active_key().version, 7);
        assert_eq!(request.digest(), [9_u8; 32]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(WalletPaymentSignature {
            wallet_id: "wallet-1".to_owned(),
            key_version: 7,
            signer_address: PAYER.to_owned(),
            bytes: vec![4, 5, 6],
        })
    }
}

struct MockProtocol;

#[async_trait]
impl PaymentProtocol for MockProtocol {
    fn name(&self) -> &'static str {
        "mock-pay"
    }

    async fn parse_challenge(
        &self,
        _scenario_id: &str,
        _expected_resource: &str,
        _wire: &Value,
        _now: i64,
        _advertised_expires_at: Option<i64>,
    ) -> Result<PaymentChallenge> {
        Err(Error::PaymentProtocol("unused".to_owned()))
    }

    async fn validate_payment(&self, challenge: &PaymentChallenge, intent: &PaymentIntent) -> Result<()> {
        if intent.challenge_hash.as_deref() == Some(challenge.challenge_hash.as_str()) {
            Ok(())
        } else {
            Err(Error::PaymentProtocol("binding mismatch".to_owned()))
        }
    }

    async fn build_credential(
        &self,
        _challenge: &PaymentChallenge,
        _signed_payload: Value,
    ) -> Result<PaymentCredential> {
        Err(Error::PaymentProtocol("unused".to_owned()))
    }

    async fn prepare_signing(
        &self,
        _challenge: &PaymentChallenge,
        payer: &str,
        _now: i64,
    ) -> Result<ProtocolSigningRequest> {
        assert_eq!(payer, PAYER);
        Ok(ProtocolSigningRequest {
            digest: vec![9_u8; 32],
            payload: json!({"payer": payer}),
        })
    }

    async fn complete_signing(
        &self,
        challenge: &PaymentChallenge,
        request: ProtocolSigningRequest,
        signature: &[u8],
    ) -> Result<PaymentCredential> {
        assert_eq!(request.digest, [9_u8; 32]);
        assert_eq!(signature, [4, 5, 6]);
        Ok(PaymentCredential {
            protocol: "mock-pay".to_owned(),
            challenge_hash: challenge.challenge_hash.clone(),
            credential_hash: "credential-1".to_owned(),
            payload: json!({"proof": "super-secret-signature"}),
        })
    }

    async fn verify_receipt(&self, _intent: &PaymentIntent, _receipt: &PaymentReceipt) -> Result<()> {
        Err(Error::PaymentProtocol("unused".to_owned()))
    }

    async fn build_challenge(&self, _offer: &PaymentOffer, _recipient: &str, _now: i64) -> Result<PaymentChallenge> {
        Err(Error::PaymentProtocol("unused".to_owned()))
    }

    async fn verify_credential(
        &self,
        _challenge: &PaymentChallenge,
        _credential: &PaymentCredential,
        _now: i64,
    ) -> Result<ProtocolValidation> {
        Err(Error::PaymentProtocol("unused".to_owned()))
    }

    fn credential_fingerprint(&self, _challenge: &PaymentChallenge, _credential: &PaymentCredential) -> Result<String> {
        Err(Error::PaymentProtocol("unused".to_owned()))
    }

    async fn settle_payment(
        &self,
        _challenge: &PaymentChallenge,
        _credential: &PaymentCredential,
    ) -> Result<ProtocolSettlement> {
        Err(Error::PaymentProtocol("unused".to_owned()))
    }
}

fn fixture() -> OutboundSigningAuthorization {
    let asset = asset();
    OutboundSigningAuthorization {
        identity: signing_identity(&asset),
        policy: signing_policy(asset),
        wallet: signing_wallet(),
        active_key: signing_key(),
    }
}

fn asset() -> AssetIdentity {
    AssetIdentity {
        network: "eip155:31337".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    }
}

fn signing_identity(asset: &AssetIdentity) -> OutboundSigningIdentity {
    let challenge = PaymentChallenge {
        scenario_id: "scenario-1".to_owned(),
        offer_id: None,
        resource: "https://merchant.example/mcp/realtime".to_owned(),
        protocol: "mock-pay".to_owned(),
        asset: asset.clone(),
        amount: Amount::new(1_000_000),
        recipient: RECIPIENT.to_owned(),
        nonce: "nonce-1".to_owned(),
        challenge_hash: "challenge-1".to_owned(),
        expires_at: 1_000,
        payload: json!({"wire": "public-challenge"}),
    };
    OutboundSigningIdentity {
        client: PaymentClient {
            id: "client-1".to_owned(),
            name: "QuantStream".to_owned(),
            auth_principal: "client-principal".to_owned(),
            enabled: true,
            created_at: 1,
            updated_at: 2,
        },
        scenario: PaymentScenario {
            id: "scenario-1".to_owned(),
            client_id: "client-1".to_owned(),
            name: "News ingestion".to_owned(),
            enabled: true,
            created_at: 3,
            updated_at: 4,
        },
        intent: PaymentIntent {
            id: "intent-1".to_owned(),
            scenario_id: "scenario-1".to_owned(),
            run_id: "run-1".to_owned(),
            wallet_id: Some("wallet-1".to_owned()),
            policy_id: Some("policy-1".to_owned()),
            offer_id: None,
            direction: PaymentDirection::Outbound,
            protocol: "mock-pay".to_owned(),
            asset: asset.clone(),
            recipient: RECIPIENT.to_owned(),
            amount: Amount::new(1_000_000),
            settled_amount: Amount::ZERO,
            resource: challenge.resource.clone(),
            challenge_hash: Some(challenge.challenge_hash.clone()),
            request_hash: "request-1".to_owned(),
            status: PaymentStatus::Authorized,
            reservation_status: ReservationStatus::Reserved,
            reserved_amount: Amount::new(1_000_000),
            tx_hash: None,
            settlement_ref: None,
            next_reconcile_at: None,
            created_at: 10,
            updated_at: 20,
            expires_at: Some(challenge.expires_at),
            settled_at: None,
        },
        challenge,
    }
}

fn signing_policy(asset: AssetIdentity) -> PaymentPolicy {
    PaymentPolicy {
        id: "policy-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        wallet_ids: vec!["wallet-1".to_owned()],
        protocols: vec!["mock-pay".to_owned()],
        networks: vec!["eip155:31337".to_owned()],
        assets: vec![asset],
        budget_unit: BudgetUnit {
            asset: "USDC".to_owned(),
            decimals: 6,
        },
        allowed_recipients: vec![RECIPIENT.to_owned()],
        allowed_origins: vec!["https://merchant.example".to_owned()],
        inbound_limits: None,
        outbound_limits: Some(PaymentPolicyLimits {
            max_per_payment: Amount::new(2_000_000),
            max_per_day: Amount::new(5_000_000),
            approval_threshold: Some(Amount::new(2_000_000)),
            approval_enabled: true,
            approval_overage: Some(Amount::new(50_000_000)),
        }),
        enabled: true,
        created_at: 5,
        updated_at: 6,
    }
}

fn signing_wallet() -> PaymentWallet {
    PaymentWallet {
        wallet: Wallet {
            id: "wallet-1".to_owned(),
            algorithm: "secp256k1".to_owned(),
            address: PAYER.to_owned(),
            public_key: "public-key-1".to_owned(),
            created_at: 7,
        },
        custody: WalletCustody::Local,
        enabled: true,
        active_key_version: Some(7),
        budget_unit: Some(BudgetUnit {
            asset: "USDC".to_owned(),
            decimals: 6,
        }),
        max_per_payment: Some(Amount::new(10_000_000)),
        max_per_day: Some(Amount::new(100_000_000)),
        updated_at: 8,
    }
}

fn signing_key() -> EncryptedWalletKeyVersion {
    EncryptedWalletKeyVersion {
        wallet_id: "wallet-1".to_owned(),
        version: 7,
        algorithm: "secp256k1".to_owned(),
        address: PAYER.to_owned(),
        public_key: "public-key-1".to_owned(),
        encryption_scheme: WALLET_KEY_ENCRYPTION_SCHEME.to_owned(),
        kek_id: "kek-1".to_owned(),
        wrapped_data_key: vec![1],
        wrap_nonce: new_uuid_nonce().to_vec(),
        key_ciphertext: vec![3],
        key_nonce: new_uuid_nonce().to_vec(),
        aad: vec![5],
        status: WalletKeyStatus::Active,
        activated_at: 9,
        retired_at: None,
        created_at: 9,
    }
}

fn handler(
    authorization: OutboundSigningAuthorization,
    asset_balance: Amount,
    native_balance: Amount,
) -> (
    OutboundSigningHandler,
    Arc<MockStorage>,
    Arc<MockBalances>,
    Arc<MockSigner>,
) {
    let storage = Arc::new(MockStorage::new(OutboundSigningRead::Authorized(Box::new(
        authorization,
    ))));
    let balances = Arc::new(MockBalances {
        snapshot: crate::handler::WalletBalanceSnapshot {
            asset_available: asset_balance,
            native_available: native_balance,
            minimum_native_required: Amount::new(10),
        },
        calls: AtomicUsize::new(0),
    });
    let signer = Arc::new(MockSigner {
        calls: AtomicUsize::new(0),
    });
    let protocol: Arc<dyn PaymentProtocol> = Arc::new(MockProtocol);
    let protocols = Arc::new(ProtocolRegistry::new([protocol]).unwrap());
    let protector: Arc<dyn SensitiveMaterialProtector> =
        Arc::new(Aes256GcmMaterialProtector::new("recovery-v1", [("recovery-v1".to_owned(), [3_u8; 32])]).unwrap());
    let handler = OutboundSigningHandler::new(OutboundSigningDependencies {
        storage: storage.clone(),
        protocols,
        balances: balances.clone(),
        signer: signer.clone(),
        material_protector: protector,
    });
    (handler, storage, balances, signer)
}

#[tokio::test]
async fn signs_once_then_replays_the_sealed_credential() {
    let authorization = fixture();
    let command = SignAuthorizedPayment {
        intent_id: authorization.identity.intent.id.clone(),
        challenge: authorization.identity.challenge.clone(),
    };
    let (handler, storage, balances, signer) = handler(authorization, Amount::new(2_000_000), Amount::new(20));

    let first = handler.sign("client-principal", &command, NOW).await.unwrap();
    assert!(!first.replayed);
    assert_eq!(first.intent.status, PaymentStatus::Signed);
    assert_eq!(first.credential.credential_hash, "credential-1");

    let replay = handler.sign("client-principal", &command, NOW + 1).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.credential, first.credential);
    assert_eq!(signer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(balances.calls.load(Ordering::SeqCst), 1);
    assert_eq!(storage.commits.load(Ordering::SeqCst), 1);

    let captured = storage.captured.lock().unwrap();
    let captured = captured.as_ref().unwrap();
    assert_eq!(captured.guard.expected_key_version, 7);
    assert_eq!(captured.events[0].event_type, PaymentEventType::SignRequested);
    assert_eq!(captured.events[1].event_type, PaymentEventType::Signed);
    assert!(!captured.events[1]
        .payload
        .to_string()
        .contains("super-secret-signature"));
    assert_eq!(captured.outbox[0].run_id, "run-1");
    assert!(!captured.outbox[0]
        .payload
        .to_string()
        .contains("super-secret-signature"));
}

#[tokio::test]
async fn insufficient_balances_fail_before_custody() {
    for (asset, native_balance) in [
        (Amount::new(999_999), Amount::new(20)),
        (Amount::new(2_000_000), Amount::new(9)),
    ] {
        let authorization = fixture();
        let command = SignAuthorizedPayment {
            intent_id: authorization.identity.intent.id.clone(),
            challenge: authorization.identity.challenge.clone(),
        };
        let (handler, storage, _balances, signer) = handler(authorization, asset, native_balance);
        assert!(matches!(
            handler.sign("client-principal", &command, NOW).await,
            Err(Error::PolicyDenied(_))
        ));
        assert_eq!(signer.calls.load(Ordering::SeqCst), 0);
        assert_eq!(storage.commits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn full_challenge_mismatch_fails_before_balance_or_signing() {
    let authorization = fixture();
    let mut challenge = authorization.identity.challenge.clone();
    challenge.payload = json!({"wire": "tampered"});
    let command = SignAuthorizedPayment {
        intent_id: authorization.identity.intent.id.clone(),
        challenge,
    };
    let (handler, storage, balances, signer) = handler(authorization, Amount::new(2_000_000), Amount::new(20));
    assert!(matches!(
        handler.sign("client-principal", &command, NOW).await,
        Err(Error::PaymentConflict(_))
    ));
    assert_eq!(balances.calls.load(Ordering::SeqCst), 0);
    assert_eq!(signer.calls.load(Ordering::SeqCst), 0);
    assert_eq!(storage.commits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stale_or_non_active_wallet_key_fails_closed() {
    let mut authorization = fixture();
    authorization.active_key.status = WalletKeyStatus::Retired;
    authorization.active_key.retired_at = Some(50);
    let command = SignAuthorizedPayment {
        intent_id: authorization.identity.intent.id.clone(),
        challenge: authorization.identity.challenge.clone(),
    };
    let (handler, storage, balances, signer) = handler(authorization, Amount::new(2_000_000), Amount::new(20));
    assert!(matches!(
        handler.sign("client-principal", &command, NOW).await,
        Err(Error::PolicyDenied(_))
    ));
    assert_eq!(balances.calls.load(Ordering::SeqCst), 0);
    assert_eq!(signer.calls.load(Ordering::SeqCst), 0);
    assert_eq!(storage.commits.load(Ordering::SeqCst), 0);
}
