from __future__ import annotations

import json
import re

from common.context import VerificationContext
from common.payer_identity import PAYER_IDENTITIES
from common.project import E2E_ROOT, REPOSITORY_ROOT


REQUIRED = (
    "runner.py",
    "deploy/namespaces.yaml",
    "common/deploy/base.py",
    "common/deploy/docker.py",
    "common/deploy/kubernetes.py",
    "common/deploy/kubernetes_deployer.py",
    "common/payer_identity.py",
    "verifier/payment/s20_full.py",
    "verifier/payment/s21_payer_signing.py",
    "verifier/payment/s22_payee_settlement.py",
    "verifier/payment/s23_balance_sync.py",
    "verifier/payment/s24_gateway_balance.py",
    "verifier/payment/s25_mpp_tempo.py",
    "verifier/payment/s26_x402_solana.py",
    "verifier/payment/s30_matrix.py",
    "verifier/web/s40_dashboard.py",
    "deploy/anvil/docker-compose.yaml",
    "deploy/solana/docker-compose.yaml",
    "deploy/facilitator/docker-compose.yaml",
    "deploy/mock-payee-marketpulse-golang-service/main.go",
    "deploy/mock-payee-marketpulse-golang-service/pkg/server.go",
    "deploy/mock-payee-marketpulse-golang-service/pkg/route/feed.go",
    "deploy/mock-payee-marketpulse-golang-service/pkg/handler/feed.go",
    "deploy/mock-payee-marketpulse-golang-service/pkg/storage/feed.go",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/Application.java",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/server/MarketPulseServer.java",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/route/FeedController.java",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/handler/FeedHandler.java",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/storage/FeedSQLiteStorage.java",
    "deploy/mock-payee-marketpulse-python-service/app/main.py",
    "deploy/mock-payee-marketpulse-python-service/app/server.py",
    "deploy/mock-payee-marketpulse-python-service/app/route/feed.py",
    "deploy/mock-payee-marketpulse-python-service/app/handler/feed.py",
    "deploy/mock-payee-marketpulse-python-service/app/storage/feed.py",
    "deploy/mock-payee-marketpulse-rust-service/src/lib.rs",
    "deploy/mock-payee-marketpulse-rust-service/src/main.rs",
    "deploy/mock-payee-marketpulse-rust-service/src/server.rs",
    "deploy/mock-payee-marketpulse-rust-service/src/route/feed.rs",
    "deploy/mock-payee-marketpulse-rust-service/src/handler/feed.rs",
    "deploy/mock-payee-marketpulse-rust-service/src/storage/feed.rs",
    "deploy/mock-payee-marketpulse-typescript-service/src/main.ts",
    "deploy/mock-payee-marketpulse-typescript-service/src/server.ts",
    "deploy/mock-payee-marketpulse-typescript-service/src/route/feed.ts",
    "deploy/mock-payee-marketpulse-typescript-service/src/handler/feed.ts",
    "deploy/mock-payee-marketpulse-typescript-service/src/storage/feed.ts",
    "deploy/mock-payer-quantstream-ingest-ui/src/server.mjs",
    "deploy/mock-payer-quantstream-ingest-service-golang/pkg/route/ingestion.go",
    "deploy/mock-payer-quantstream-ingest-service-golang/pkg/handler/ingestion.go",
    "deploy/mock-payer-quantstream-ingest-service-golang/pkg/agent/mcp.go",
    "deploy/mock-payer-quantstream-ingest-service-golang/pkg/job/ingestion_job.go",
    "deploy/mock-payer-quantstream-ingest-service-rust/src/lib.rs",
    "deploy/mock-payer-quantstream-ingest-service-rust/src/route/ingestion.rs",
    "deploy/mock-payer-quantstream-ingest-service-rust/src/handler/ingestion.rs",
    "deploy/mock-payer-quantstream-ingest-service-rust/src/agent/mcp.rs",
    "deploy/mock-payer-quantstream-ingest-service-rust/src/job/ingestion_job.rs",
    "deploy/mock-payer-quantstream-ingest-service-python/app/main.py",
    "deploy/mock-payer-quantstream-ingest-service-python/app/route/ingestion.py",
    "deploy/mock-payer-quantstream-ingest-service-python/app/handler/ingestion.py",
    "deploy/mock-payer-quantstream-ingest-service-python/app/agent/mcp.py",
    "deploy/mock-payer-quantstream-ingest-service-python/app/job/ingestion_job.py",
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/Application.java",
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/route/IngestionController.java",
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/handler/IngestionHandler.java",
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/agent/MCPHelper.java",
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/job/IngestionJob.java",
    "deploy/mock-payer-quantstream-ingest-service-typescript/src/main.ts",
    "deploy/mock-payer-quantstream-ingest-service-typescript/src/route/ingestion.ts",
    "deploy/mock-payer-quantstream-ingest-service-typescript/src/handler/ingestion.ts",
    "deploy/mock-payer-quantstream-ingest-service-typescript/src/agent/mcp.ts",
    "deploy/mock-payer-quantstream-ingest-service-typescript/src/job/ingestion_job.ts",
    "common/contract/payment-mqtt.schema.json",
    "common/contract/scenario.json",
    "docker-compose.yaml",
)

DEPLOY_COMPONENTS = (
    "anvil",
    "emqx",
    "evm",
    "facilitator",
    "jaeger",
    "mock-payee-marketpulse-golang-service",
    "mock-payee-marketpulse-java-service",
    "mock-payee-marketpulse-python-service",
    "mock-payee-marketpulse-rust-service",
    "mock-payee-marketpulse-typescript-service",
    "mock-payer-quantstream-ingest-service-golang",
    "mock-payer-quantstream-ingest-service-java",
    "mock-payer-quantstream-ingest-service-python",
    "mock-payer-quantstream-ingest-service-rust",
    "mock-payer-quantstream-ingest-service-typescript",
    "mock-payer-quantstream-ingest-ui",
    "postgres",
    "solana",
    "tempo",
)

E2E_NAMESPACE_GROUPS = {
    "e2e-agentvault-infra-system": ("postgres", "emqx", "jaeger"),
    "e2e-agentvault-payment-system": ("anvil", "evm", "facilitator", "solana", "tempo"),
    "e2e-agentvault-mock-payee": (
        "mock-payee-marketpulse-golang-service",
        "mock-payee-marketpulse-java-service",
        "mock-payee-marketpulse-python-service",
        "mock-payee-marketpulse-rust-service",
        "mock-payee-marketpulse-typescript-service",
    ),
    "e2e-agentvault-mock-payer": (
        "mock-payer-quantstream-ingest-service-golang",
        "mock-payer-quantstream-ingest-service-java",
        "mock-payer-quantstream-ingest-service-python",
        "mock-payer-quantstream-ingest-service-rust",
        "mock-payer-quantstream-ingest-service-typescript",
        "mock-payer-quantstream-ingest-ui",
    ),
}

REQUIRED_E2E_NAMESPACES = frozenset({
    "e2e-agentvault-system",
    "e2e-agentvault-authguard-system",
    *E2E_NAMESPACE_GROUPS,
})

COMPONENT_MANIFESTS = (
    "docker-compose.yaml",
    "helm/Chart.yaml",
    "helm/values.yaml",
    "helm/templates/resources.yaml",
)

SDK_SOURCE_ALLOWLISTS = {
    "golang/pkg": {
        "agentvault.go", "config/config.go",
        "model/approval.go", "model/base.go", "model/payee.go", "model/payer.go",
        "payer/payer.go", "payer/standard.go", "payee/payee.go", "payee/standard.go",
        "payee/gin.go", "transport/transport.go", "transport/grpc.go", "transport/mqtt.go",
        "transport/factory.go", "protocol/adapter.go", "protocol/x402.go", "protocol/mpp.go",
    },
    "rust/src": {
        "lib.rs", "agentvault.rs", "config/config.rs",
        "model/approval.rs", "model/base.rs", "model/payee.rs", "model/payer.rs",
        "payer/payer.rs", "payer/standard.rs", "payee/payee.rs", "payee/standard.rs",
        "payee/axum.rs", "transport/transport.rs", "transport/grpc.rs", "transport/mqtt.rs",
        "transport/factory.rs", "protocol/adapter.rs", "protocol/x402.rs", "protocol/mpp.rs",
    },
    "python/agentvault": {
        "__init__.py", "agentvault.py", "config/__init__.py", "config/config.py",
        "model/__init__.py", "model/approval.py", "model/base.py", "model/payee.py", "model/payer.py",
        "payer/__init__.py", "payer/payer.py", "payer/standard.py", "payee/__init__.py",
        "payee/payee.py", "payee/standard.py", "payee/fast.py", "transport/__init__.py",
        "transport/transport.py", "transport/grpc.py", "transport/mqtt.py",
        "transport/factory.py", "protocol/__init__.py", "protocol/adapter.py",
        "protocol/x402.py", "protocol/mpp.py",
    },
    "typescript/src/agentvault": {
        "agentvault.ts", "config/config.ts", "model/approval.ts", "model/base.ts",
        "model/payee.ts", "model/payer.ts", "payer/payer.ts", "payer/standard.ts",
        "payee/payee.ts", "payee/standard.ts", "payee/express.ts", "transport/transport.ts",
        "transport/grpc.ts", "transport/mqtt.ts",
        "transport/factory.ts", "protocol/adapter.ts", "protocol/x402.ts", "protocol/mpp.ts",
    },
    "java/src/io/agentvault/sdk": {
        "AgentVault.java", "config/Config.java", "model/Approval.java",
        "model/Base.java", "model/Payee.java", "model/Payer.java", "payer/PayerMiddleware.java",
        "payer/SpringInterceptorPayerMiddleware.java", "payer/JdkHttpPayerMiddleware.java",
        "payee/PayeeMiddleware.java",
        "payee/StandardPayeeMiddleware.java", "payee/SpringInterceptorPayeeMiddleware.java",
        "transport/VaultTransport.java", "transport/GrpcVaultTransport.java", "transport/MqttVaultTransport.java",
        "transport/VaultTransportFactory.java", "protocol/PaymentProtocol.java",
        "protocol/PaymentProtocolFactory.java", "protocol/ProtocolRegistry.java",
        "protocol/X402Adapter.java", "protocol/X402ExactPayload.java", "protocol/MppAdapter.java",
    },
}

SDK_TEST_ROOTS = {
    "golang/pkg": "golang/tests",
    "rust/src": "rust/tests",
    "python/agentvault": "python/tests/agentvault",
    "typescript/src/agentvault": "typescript/tests/agentvault",
    "java/src/io/agentvault/sdk": "java/tests/io/agentvault/sdk",
}

INGESTOR_SOURCE_ALLOWLISTS = {
    "deploy/mock-payer-quantstream-ingest-service-golang": {
        "main.go", "pkg/agent/mcp.go", "pkg/handler/ingestion.go",
        "pkg/job/ingestion_job.go", "pkg/route/ingestion.go",
    },
    "deploy/mock-payer-quantstream-ingest-service-rust/src": {
        "lib.rs", "main.rs", "agent/mod.rs", "agent/mcp.rs", "handler/mod.rs",
        "handler/ingestion.rs", "job/mod.rs", "job/ingestion_job.rs", "route/mod.rs",
        "route/ingestion.rs",
    },
    "deploy/mock-payer-quantstream-ingest-service-python/app": {
        "__init__.py", "main.py", "agent/__init__.py", "agent/mcp.py",
        "handler/__init__.py", "handler/ingestion.py", "job/__init__.py", "job/ingestion_job.py",
        "route/__init__.py", "route/ingestion.py",
    },
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e": {
        "Application.java", "agent/MCPHelper.java", "handler/IngestionHandler.java",
        "job/IngestionJob.java", "route/IngestionController.java",
    },
    "deploy/mock-payer-quantstream-ingest-service-typescript/src": {
        "main.ts", "agent/mcp.ts", "handler/ingestion.ts", "job/ingestion_job.ts",
        "route/ingestion.ts",
    },
}

INGESTOR_TEST_ROOTS = {
    "deploy/mock-payer-quantstream-ingest-service-golang":
        "deploy/mock-payer-quantstream-ingest-service-golang/tests",
    "deploy/mock-payer-quantstream-ingest-service-rust/src":
        "deploy/mock-payer-quantstream-ingest-service-rust/tests",
    "deploy/mock-payer-quantstream-ingest-service-python/app":
        "deploy/mock-payer-quantstream-ingest-service-python/tests",
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e":
        "deploy/mock-payer-quantstream-ingest-service-java/tests/io/agentvault/e2e",
    "deploy/mock-payer-quantstream-ingest-service-typescript/src":
        "deploy/mock-payer-quantstream-ingest-service-typescript/tests",
}

PAYEE_SOURCE_ALLOWLISTS = {
    "deploy/mock-payee-marketpulse-golang-service": {
        "main.go", "pkg/server.go", "pkg/handler/feed.go", "pkg/route/feed.go", "pkg/storage/feed.go",
    },
    "deploy/mock-payee-marketpulse-rust-service/src": {
        "lib.rs", "main.rs", "server.rs", "handler/mod.rs", "handler/feed.rs", "route/mod.rs",
        "route/feed.rs", "storage/mod.rs", "storage/feed.rs",
    },
    "deploy/mock-payee-marketpulse-python-service/app": {
        "__init__.py", "main.py", "server.py", "handler/__init__.py", "handler/feed.py",
        "route/__init__.py", "route/feed.py", "storage/__init__.py", "storage/feed.py",
    },
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e": {
        "Application.java", "server/MarketPulseServer.java", "handler/FeedHandler.java", "route/FeedController.java",
        "storage/FeedSQLiteStorage.java",
    },
    "deploy/mock-payee-marketpulse-typescript-service/src": {
        "main.ts", "server.ts", "handler/feed.ts", "route/feed.ts", "storage/feed.ts",
    },
}

PAYEE_TEST_ROOTS = {
    "deploy/mock-payee-marketpulse-golang-service":
        "deploy/mock-payee-marketpulse-golang-service/tests",
    "deploy/mock-payee-marketpulse-rust-service/src":
        "deploy/mock-payee-marketpulse-rust-service/tests",
    "deploy/mock-payee-marketpulse-python-service/app":
        "deploy/mock-payee-marketpulse-python-service/tests",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e":
        "deploy/mock-payee-marketpulse-java-service/tests/io/agentvault/e2e",
    "deploy/mock-payee-marketpulse-typescript-service/src":
        "deploy/mock-payee-marketpulse-typescript-service/tests",
}

SOURCE_SUFFIXES = {".go", ".rs", ".py", ".ts", ".java"}
GENERATED_DIRECTORIES = {"node_modules", "target", "dist", "build", "__pycache__"}
FORBIDDEN_SDK_SYMBOLS = (
    "PaymentClient", "PayeeClient", "ApprovalClient", "PayerMiddlewareConfig",
    "PaymentMiddleware", "PaymentMqttTransport", "payeeMiddleware(", ".forFetch(",
    ".httpx(", ".reqwest(",
)

FORBIDDEN_SDK_PATTERNS = (
    r"\bAssetID\b",
    r"\bAssetId\b",
    r"\bkind\s*:\s*['\"]payment_required['\"]",
)

SDK_MODEL_CONTRACTS = {
    "golang/pkg": {
        "model/base.go": ("type PaymentDirection", "type PaymentStatus", "type ReservationStatus",
                          "type AssetIdentity", "type PaymentChallenge", "type HTTPCredential",
                          "type PaymentCredential", "type PaymentIntent", "type PaymentReceipt"),
        "model/approval.go": ("type ApprovalStatus", "type ApprovalDecisionKind", "type ApprovalRequest",
                              "type ApprovalQuery", "type ApprovalPage", "type SubmitApprovalDecisionRequest"),
        "model/payer.go": ("type AuthorizationOutcome", "type PaymentRequired", "type AuthorizePaymentRequest",
                           "type PaymentAuthorizationResult", "type AuthorizePaymentResponse",
                           "type ReportOutboundResponseRequest", "type ReportOutboundResponseResponse",
                           "type BalanceQuery", "type PaymentBalance", "type PayerRequestContext",
                           "type PayerHTTPRequest", "type PayerHTTPResponse", "type AuthorizedPayment"),
        "model/payee.go": ("type ResolveOfferRequest", "type PaymentOffer", "type PayeeOperation",
                           "type CreateChallengeRequest", "type VerifyPaymentRequest", "type PaymentVerification",
                           "type SettlePaymentRequest", "type SettledIncomingPayment",
                           "type HTTPProtectionRequest", "type MCPProtectionRequest", "type ProtectionOutcome"),
    },
    "rust/src": {
        "model/base.rs": ("pub enum PaymentDirection", "pub enum PaymentStatus", "pub enum ReservationStatus",
                          "pub struct AssetIdentity", "pub struct PaymentChallenge", "pub struct HttpCredential",
                          "pub struct PaymentCredential", "pub struct PaymentIntent", "pub struct PaymentReceipt"),
        "model/approval.rs": ("pub enum ApprovalStatus", "pub enum ApprovalDecisionKind",
                              "pub struct ApprovalRequest", "pub struct ApprovalQuery", "pub struct ApprovalPage",
                              "pub struct SubmitApprovalDecisionRequest"),
        "model/payer.rs": ("pub struct PaymentRequired", "pub struct AuthorizePaymentRequest",
                           "pub enum PaymentAuthorizationResult", "pub struct AuthorizePaymentResponse",
                           "pub struct ReportOutboundResponseRequest", "pub struct ReportOutboundResponseResponse",
                           "pub struct BalanceQuery", "pub struct PaymentBalance", "pub struct PayerRequestContext",
                           "pub struct PayerHttpRequest", "pub struct PayerHttpResponse",
                           "pub struct AuthorizedPayment"),
        "model/payee.rs": ("pub struct ResolveOfferRequest", "pub struct PaymentOffer", "pub struct PayeeOperation",
                           "pub struct CreateChallengeRequest", "pub struct VerifyPaymentRequest",
                           "pub struct PaymentVerification", "pub struct SettlePaymentRequest",
                            "pub struct SettledIncomingPayment",
                           "pub struct HttpProtectionRequest", "pub struct McpProtectionRequest",
                           "pub enum ProtectionOutcome"),
    },
    "python/agentvault": {
        "model/base.py": ("class PaymentDirection", "class PaymentStatus", "class ReservationStatus",
                          "class AssetIdentity", "class PaymentChallenge", "class HttpCredential",
                          "class PaymentCredential", "class PaymentIntent", "class PaymentReceipt"),
        "model/approval.py": ("class ApprovalStatus", "class ApprovalDecisionKind", "class ApprovalRequest",
                              "class ApprovalQuery", "class ApprovalPage", "class SubmitApprovalDecisionRequest"),
        "model/payer.py": ("class AuthorizationOutcome", "class PaymentRequired", "class AuthorizePaymentRequest",
                           "class PaymentAuthorizationResult", "class AuthorizePaymentResponse",
                           "class ReportOutboundResponseRequest", "class ReportOutboundResponseResponse",
                           "class BalanceQuery", "class PaymentBalance", "class PayerRequestContext",
                           "class PayerHttpRequest", "class PayerHttpResponse", "class AuthorizedPayment"),
        "model/payee.py": ("class ResolveOfferRequest", "class PaymentOffer", "class PayeeOperation",
                           "class CreateChallengeRequest", "class VerifyPaymentRequest", "class PaymentVerification",
                           "class SettlePaymentRequest", "class SettledIncomingPayment",
                           "class HttpProtectionRequest", "class McpProtectionRequest", "class ProtectionOutcome"),
    },
    "typescript/src/agentvault": {
        "model/base.ts": ("type PaymentDirection", "type PaymentStatus", "type ReservationStatus",
                          "interface AssetIdentity", "interface PaymentChallenge", "interface HttpCredential",
                          "class PaymentCredential", "interface PaymentIntent", "interface PaymentReceipt"),
        "model/approval.ts": ("type ApprovalStatus", "type ApprovalDecisionKind", "interface ApprovalRequest",
                              "interface ApprovalQuery", "interface ApprovalPage",
                              "interface SubmitApprovalDecisionRequest"),
        "model/payer.ts": ("type AuthorizationOutcome", "interface PaymentRequired",
                           "interface AuthorizePaymentRequest", "interface AuthorizePaymentResponse",
                           "type ReportOutboundResponseRequest", "interface ReportOutboundResponseResponse",
                           "interface BalanceQuery", "interface PaymentBalance", "class PayerRequestContext",
                           "interface PayerHttpRequest", "interface PayerHttpResponse",
                           "interface AuthorizedPayment"),
        "model/payee.ts": ("interface ResolveOfferRequest", "interface PaymentOffer", "interface PayeeOperation",
                           "interface CreateChallengeRequest", "interface VerifyPaymentRequest",
                           "interface PaymentVerification", "interface SettlePaymentRequest",
                           "interface SettledIncomingPayment",
                           "interface HttpProtectionRequest", "interface McpProtectionRequest",
                           "type ProtectionOutcome"),
    },
    "java/src/io/agentvault/sdk": {
        "model/Base.java": ("enum PaymentDirection", "enum PaymentStatus", "enum ReservationStatus",
                            "record AssetIdentity", "record PaymentChallenge", "record HttpCredential",
                            "record PaymentCredential", "record PaymentIntent", "record PaymentReceipt"),
        "model/Approval.java": ("enum ApprovalStatus", "enum ApprovalDecisionKind", "record ApprovalRequest",
                                "record ApprovalQuery", "record ApprovalPage",
                                "record SubmitApprovalDecisionRequest"),
        "model/Payer.java": ("enum AuthorizationOutcome", "record PaymentRequired",
                             "record AuthorizePaymentRequest", "record PaymentAuthorizationResult",
                             "record AuthorizePaymentResponse", "record ReportOutboundResponseRequest",
                             "record ReportOutboundResponseResponse", "record BalanceQuery",
                             "record PaymentBalance", "record PayerRequestContext", "record PayerHttpRequest",
                             "record PayerHttpResponse", "record AuthorizedPayment"),
        "model/Payee.java": ("record ResolveOfferRequest", "record PaymentOffer", "record PayeeOperation",
                             "record CreateChallengeRequest", "record VerifyPaymentRequest",
                             "record PaymentVerification", "record SettlePaymentRequest",
                             "record SettledIncomingPayment", "record HttpProtectionRequest",
                             "record McpProtectionRequest", "interface ProtectionOutcome"),
    },
}

SDK_LOGICAL_CONTRACTS = {
    "golang/pkg": {
        "agentvault.go": ("type AgentVault", "Payer(", "Payee(", "GetPendingApprovals", "SubmitApprovalDecision"),
        "model/payer.go": ("type PayerRequestContext", "type PayerHTTPRequest", "type PayerHTTPResponse", "type AuthorizedPayment", "type BalanceQuery"),
        "model/payee.go": ("type PayeeOperation", "type ResolveOfferRequest", "type ProtectionOutcome"),
        "payer/payer.go": ("type PayerMiddleware", "Authorize(", "Report(", "CredentialHeader(", "GetPayment(", "GetReceipt(", "GetBalance(",
                            "protocolHint != \"\" && intent.Protocol != protocolHint", "intent.ScenarioID != m.config.ScenarioID",
                            "intent.WalletID == nil", "intent.PolicyID == \"\"",
                            "intent.RunID != operation.RunID", "DeliveryRetries", "StatusServiceUnavailable"),
        "payer/standard.go": ("type StandardPayerMiddleware", "*PayerMiddleware", "RoundTrip(", "StatusPaymentRequired", "CheckRedirect", "m.Deliver("),
        "payer/payer_test.go": ("ForwardsCapturedProtocolHintWithoutLocalProjection", "payment-protocol", "mpp"),
        "payee/payee.go": ("type PayeeMiddleware", "ErrInvalidPayment", "Challenge(", "Protect(", "ResolveOffer(", "GetReceipt(",
                            "OperationContext: settlementContext",
                            "intent.Asset != challenge.Asset", "payment.Intent.Status != model.PaymentStatusSettled",
                            "payment.Receipt.Status != model.PaymentStatusSettled"),
        "payee/gin.go": ("type GinPayeeMiddleware", "Handler() gin.HandlerFunc", "NewMCPToolRoute", "parseMCPCall", "errors.Is(err, ErrInvalidPayment)"),
        "payee/standard.go": ("type StandardPayeeMiddleware", "*PayeeMiddleware", "NewStandardMCPToolRoute", "parseMCPCall", "errors.Is(err, ErrInvalidPayment)"),
        "payee/payee_test.go": ("RejectsInvalidPaymentBeforeSettlement", "/api/v1/payee/verify", "ErrInvalidPayment"),
        "transport/transport.go": ("type Transport interface",),
        "transport/grpc.go": ("type GrpcVaultTransport",),
        "transport/mqtt.go": ("type MqttVaultTransport", "MaxResponseBytes"),
    },
    "rust/src": {
        "agentvault.rs": ("pub struct AgentVault", "pub fn payer", "pub fn payee", "get_pending_approvals", "submit_approval_decision"),
        "model/payer.rs": ("pub struct PayerRequestContext", "pub struct PayerHttpRequest", "pub struct PayerHttpResponse", "pub struct AuthorizedPayment", "pub struct BalanceQuery"),
        "model/payee.rs": ("pub struct PayeeOperation", "pub struct ResolveOfferRequest", "pub enum ProtectionOutcome"),
        "payer/payer.rs": ("pub struct PayerMiddleware", "pub async fn authorize", "pub async fn report", "pub fn credential_header", "pub async fn get_payment", "pub async fn get_receipt", "pub async fn get_balance",
                            "protocol_hint.is_some_and", "intent.scenario_id != config.scenario_id",
                            "intent.wallet_id.as_deref().is_none_or", "intent.policy_id.as_deref().is_none_or",
                            "intent.run_id != context.run_id", "delivery_retries", "view.status != 503"),
        "payer/standard.rs": ("pub struct ReqwestPayerMiddleware", "impl<T> Middleware", "Policy::none", ".deliver(&context"),
        "payer/payer_test.rs": ("payer_forwards_captured_protocol_hint_without_local_projection", "payment-protocol", "mpp"),
        "payee/payee.rs": ("pub struct PayeeMiddleware", "pub async fn challenge", "pub async fn protect", "pub async fn resolve_offer", "pub async fn get_receipt",
                            "OperationContext::new(operation.scenario_id.clone(), operation.run_id.clone())",
                            "intent.asset != challenge.asset", "payment.intent.status != crate::PaymentStatus::Settled",
                            "payment.receipt.status != crate::PaymentStatus::Settled"),
        "payee/axum.rs": ("pub struct AxumPayeeMiddleware", "pub fn mcp_tool", "is_mcp_tool_call"),
        "payee/payee_test.rs": ("payee_rejects_invalid_payment_before_settlement", "/api/v1/payee/verify", "status: 402"),
        "transport/transport.rs": ("pub trait VaultTransport",),
        "transport/grpc.rs": ("pub struct GrpcVaultTransport",),
        "transport/mqtt.rs": ("pub struct MqttVaultTransport", "max_response_bytes"),
    },
    "python/agentvault": {
        "agentvault.py": ("class AgentVault", "def payer", "def payee", "def get_pending_approvals", "def submit_approval_decision"),
        "model/payer.py": ("class PayerRequestContext", "class PayerHttpRequest", "class PayerHttpResponse", "class AuthorizedPayment", "class BalanceQuery"),
        "model/payee.py": ("class PayeeOperation", "class ResolveOfferRequest"),
        "payer/payer.py": ("class PayerMiddleware", "def authorize", "def report", "def credential_header", "def get_payment", "def get_receipt", "def get_balance",
                            "required.protocol_hint is not None and intent.protocol != required.protocol_hint",
                            "intent.scenario_id != self.config.scenario_id", "not intent.wallet_id",
                            "not intent.policy_id", "intent.run_id != operation.run_id",
                            "self.config.delivery_retries", "response_view.status != 503"),
        "payer/standard.py": ("class HttpxPayerMiddleware", "async def request", "follow_redirects=False", "stream=True", "_read_bounded", "self.core.deliver("),
        "payer/payer_test.py": ("test_captured_protocol_hint_is_checked_against_vault_result", "payment-protocol", "mpp"),
        "payee/payee.py": ("class PayeeMiddleware", "def challenge", "def protect", "def resolve_offer", "def get_receipt",
                            "operation.settlement_context(), verification.intent.id",
                            "intent.asset != challenge.asset", "intent.status.value != \"settled\"",
                            "receipt.status.value != \"settled\""),
        "payee/fast.py": ("class FastPayeeMiddleware", "async def __call__", "def mcp_tool", "def _mcp_call"),
        "payee/payee_test.py": ("test_rejects_invalid_payment_before_settlement", "/api/v1/payee/verify", "PermissionError"),
        "transport/transport.py": ("class VaultTransport",),
        "transport/grpc.py": ("class GrpcVaultTransport",),
        "transport/mqtt.py": ("class MqttVaultTransport", "max_response_bytes"),
    },
    "typescript/src/agentvault": {
        "agentvault.ts": ("class AgentVault", "payer(", "payee(", "getPendingApprovals", "submitApprovalDecision"),
        "model/payer.ts": ("class PayerRequestContext", "interface PayerHttpRequest", "interface PayerHttpResponse", "interface AuthorizedPayment", "interface BalanceQuery"),
        "model/payee.ts": ("interface PayeeOperation", "interface ResolveOfferRequest", "type ProtectionOutcome"),
        "payer/payer.ts": ("class PayerMiddleware", "async authorize", "async report", "credentialHeader", "getPayment", "getReceipt", "getBalance",
                            "protocolHint !== null && intent.protocol !== protocolHint",
                            "intent.scenario_id !== this.config.scenarioId", "!intent.wallet_id",
                            "!intent.policy_id", "intent.run_id !== operation.runId",
                            "this.config.deliveryRetries", "responseView.status !== 503"),
        "payer/standard.ts": ("class FetchPayerMiddleware", "async request", "redirect: 'manual'", "this.core.deliver("),
        "payer/payer_test.ts": ("forwards captured protocol hints without local projections", "payment-protocol", "mpp"),
        "payee/payee.ts": ("class PayeeMiddleware", "class InvalidPaymentError", "async challenge", "async protect", "resolveOffer", "getReceipt",
                            "const settlement = operationContext(operation)",
                            "!sameAsset(intent.asset, challenge.asset)", "intent.status !== 'settled'",
                            "receipt.status !== 'settled'"),
        "payee/express.ts": ("class ExpressPayeeMiddleware", "handler()", "mcpToolRoute", "mcpCall", "error instanceof InvalidPaymentError"),
        "payee/payee_test.ts": ("rejects invalid payment before settlement", "/api/v1/payee/verify", "InvalidPaymentError"),
        "transport/transport.ts": ("interface VaultTransport",),
        "transport/grpc.ts": ("class GrpcVaultTransport",),
        "transport/mqtt.ts": ("class MqttVaultTransport", "maxResponseBytes"),
    },
    "java/src/io/agentvault/sdk": {
        "AgentVault.java": ("class AgentVault", "payer(", "payee(", "getPendingApprovals", "submitApprovalDecision"),
        "model/Payer.java": ("record PayerRequestContext", "record PayerHttpRequest", "record PayerHttpResponse", "record AuthorizedPayment", "record BalanceQuery"),
        "model/Payee.java": ("record PayeeOperation", "record ResolveOfferRequest", "interface ProtectionOutcome"),
        "payer/PayerMiddleware.java": ("class PayerMiddleware", "SpringInterceptorPayerMiddleware spring()", "AuthorizedPayment authorize", "void report", "credentialHeader", "getPayment", "getReceipt", "getBalance",
                            "challenge.protocolHint() != null", "!config.scenarioId().equals(intent.scenarioId())",
                            "intent.walletId() == null || intent.walletId().isBlank()",
                            "intent.policyId() == null || intent.policyId().isBlank()",
                            "!context.runId().equals(intent.runId())", "config.deliveryRetries()", "view.status() != 503"),
        "payer/SpringInterceptorPayerMiddleware.java": ("class SpringInterceptorPayerMiddleware", "intercept(", "HttpClient.Redirect.NEVER", "value() != 402", "core.deliver("),
        "payer/PayerMiddlewareTest.java": ("forwardsCapturedProtocolHintWithoutLocalProjection", "payment-protocol", "mpp"),
        "payee/PayeeMiddleware.java": ("class PayeeMiddleware", "challenge(", "protect(", "resolveOffer", "getReceipt",
                            "new SettlePaymentRequest(settlement, verification.intent().id()",
                            "!challenge.asset().equals(intent.asset())", "payment.intent().status() != Base.PaymentStatus.SETTLED",
                            "payment.receipt().status() != Base.PaymentStatus.SETTLED"),
        "payee/SpringInterceptorPayeeMiddleware.java": ("class SpringInterceptorPayeeMiddleware", "doFilterInternal", "mcpTool", "mcpCall"),
        "payee/PayeeMiddlewareTest.java": ("rejectsInvalidPaymentBeforeSettlement", "/api/v1/payee/verify", "SecurityException"),
        "transport/VaultTransport.java": ("interface VaultTransport",),
        "transport/GrpcVaultTransport.java": ("class GrpcVaultTransport",),
        "transport/MqttVaultTransport.java": ("class MqttVaultTransport", "maxResponseBytes"),
    },
}

MIDDLEWARE_MODEL_DECLARATIONS = {
    "golang/pkg": ("payer/payer.go", ("type PayerRequestContext", "type AuthorizedPayment")),
    "rust/src": ("payer/payer.rs", ("pub struct PayerRequestContext", "pub struct AuthorizedPayment")),
    "python/agentvault": ("payer/payer.py", ("class PayerRequestContext", "class AuthorizedPayment")),
    "typescript/src/agentvault": ("payer/payer.ts", ("interface PayerHttpRequest", "interface AuthorizedPayment")),
    "java/src/io/agentvault/sdk": ("payer/PayerMiddleware.java", ("record PayerHttpRequest", "record AuthorizedPayment")),
}

JOB_CONTRACTS = {
    "golang": ("IngestYesterdayNews", "IngestRealtimeNews", "QUANTSTREAM_YESTERDAY_CRON", "QUANTSTREAM_REALTIME_CRON"),
    "rust": ("ingest_yesterday_news", "ingest_realtime_news", "QUANTSTREAM_YESTERDAY_CRON", "QUANTSTREAM_REALTIME_CRON"),
    "python": ("ingest_yesterday_news", "ingest_realtime_news", "QUANTSTREAM_YESTERDAY_CRON", "QUANTSTREAM_REALTIME_CRON"),
    "java": ("ingestYesterdayNews", "ingestRealtimeNews", "QUANTSTREAM_YESTERDAY_CRON", "QUANTSTREAM_REALTIME_CRON"),
    "typescript": ("ingestYesterdayNews", "ingestRealtimeNews", "QUANTSTREAM_YESTERDAY_CRON", "QUANTSTREAM_REALTIME_CRON"),
}

JOB_FILES = {
    "golang": "deploy/mock-payer-quantstream-ingest-service-golang/pkg/job/ingestion_job.go",
    "rust": "deploy/mock-payer-quantstream-ingest-service-rust/src/job/ingestion_job.rs",
    "python": "deploy/mock-payer-quantstream-ingest-service-python/app/job/ingestion_job.py",
    "java": "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/job/IngestionJob.java",
    "typescript": "deploy/mock-payer-quantstream-ingest-service-typescript/src/job/ingestion_job.ts",
}

MCP_FILES = {
    "golang": "deploy/mock-payer-quantstream-ingest-service-golang/pkg/agent/mcp.go",
    "rust": "deploy/mock-payer-quantstream-ingest-service-rust/src/agent/mcp.rs",
    "python": "deploy/mock-payer-quantstream-ingest-service-python/app/agent/mcp.py",
    "java": "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/agent/MCPHelper.java",
    "typescript": "deploy/mock-payer-quantstream-ingest-service-typescript/src/agent/mcp.ts",
}

MCP_CONTRACTS = {
    "golang": ("type MCPHelper", "github.com/mark3labs/mcp-go/client"),
    "rust": ("pub struct MCPHelper", "use rmcp"),
    "python": ("class MCPHelper", "from mcp import ClientSession"),
    "java": ("class MCPHelper", "io.modelcontextprotocol.client.McpClient"),
    "typescript": ("class MCPHelper", "@modelcontextprotocol/sdk/client"),
}

MCP_TRANSPORT_CONTRACTS = {
    "golang": ("AGENTVAULT_TRANSPORT", "VaultTransportConfig", "NewFromConfig",
                "agentvault.sdk.payer.transport_selected"),
    "rust": ("AGENTVAULT_TRANSPORT", "VaultTransportConfig", "AgentVault::from_config",
             "agentvault.sdk.payer.transport_selected"),
    "python": ("AGENTVAULT_TRANSPORT", "VaultTransportConfig", "AgentVault.from_config",
               "agentvault.sdk.payer.transport_selected"),
    "java": ("AGENTVAULT_TRANSPORT", "VaultTransportConfig", "AgentVault.fromConfig",
             "agentvault.sdk.payer.transport_selected"),
    "typescript": ("AGENTVAULT_TRANSPORT", "VaultTransportConfig", "AgentVault.fromConfig",
                   "agentvault.sdk.payer.transport_selected"),
}

DIRECT_TRANSPORT_CONSTRUCTORS = {
    "golang": re.compile(r"\bNew(?:Mqtt|Grpc)VaultTransport\s*\("),
    "rust": re.compile(r"\b(?:Mqtt|Grpc)VaultTransport::new\s*\("),
    "python": re.compile(r"\b(?:Mqtt|Grpc)VaultTransport\s*\("),
    "java": re.compile(r"\bnew\s+(?:Mqtt|Grpc)VaultTransport\s*\("),
    "typescript": re.compile(r"\bnew\s+(?:Mqtt|Grpc)VaultTransport\s*\("),
}

PAYER_FORBIDDEN_CHAIN_PATTERNS = (
    re.compile(r"\bethclient\b", re.IGNORECASE),
    re.compile(r"\bweb3(?:j|py)?\b", re.IGNORECASE),
    re.compile(r"\bethers(?:\.js)?\b", re.IGNORECASE),
    re.compile(r"\bsolana(?:[_-]?(?:rpc|client))?\b", re.IGNORECASE),
    re.compile(r"\banvil\b", re.IGNORECASE),
    re.compile(r"\bcast\s+(?:call|send|wallet)\b", re.IGNORECASE),
    re.compile(r"\bbalanceOf\s*\(", re.IGNORECASE),
    re.compile(r"\b(?:CHAIN|EVM|SOLANA|ANVIL)_RPC(?:_URL)?\b", re.IGNORECASE),
)

PAYEE_FRAMEWORK_CONTRACTS = {
    "deploy/mock-payee-marketpulse-golang-service/pkg/server.go": (
        "GinPayeeMiddleware", "VaultTransportConfig", "NewFromConfig",
        "NewFeedSQLiteStorage", "NewFeedHandler", "NewFeedRouter", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-rust-service/src/server.rs": (
        "AxumPayeeMiddleware", "VaultTransportConfig", "AgentVault::from_config",
        "FeedSQLiteStorage", "FeedHandler", "FeedRouter", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-python-service/app/server.py": (
        "FastAPI", "FastPayeeMiddleware", "VaultTransportConfig", "AgentVault.from_config",
        "FeedSQLiteStorage", "FeedHandler", "FeedRouter", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/server/MarketPulseServer.java": (
        "MarketPulseServer", "SpringInterceptorPayeeMiddleware", "VaultTransportConfig", "AgentVault.fromConfig",
        "FeedSQLiteStorage", "McpEndpoint", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-typescript-service/src/server.ts": (
        "ExpressPayeeMiddleware", "VaultTransportConfig", "AgentVault.fromConfig",
        "FeedSQLiteStorage", "FeedHandler", "FeedRouter", "'/mcp'",
    ),
}

PAYEE_FRAMEWORK_LANGUAGES = {
    "deploy/mock-payee-marketpulse-golang-service/pkg/server.go": "golang",
    "deploy/mock-payee-marketpulse-rust-service/src/server.rs": "rust",
    "deploy/mock-payee-marketpulse-python-service/app/server.py": "python",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/server/MarketPulseServer.java": "java",
    "deploy/mock-payee-marketpulse-typescript-service/src/server.ts": "typescript",
}

PAYEE_ROUTE_FILES = (
    "deploy/mock-payee-marketpulse-golang-service/pkg/route/feed.go",
    "deploy/mock-payee-marketpulse-rust-service/src/route/feed.rs",
    "deploy/mock-payee-marketpulse-python-service/app/route/feed.py",
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/route/FeedController.java",
    "deploy/mock-payee-marketpulse-typescript-service/src/route/feed.ts",
)

PAYER_ROUTE_FILES = (
    "deploy/mock-payer-quantstream-ingest-service-golang/pkg/route/ingestion.go",
    "deploy/mock-payer-quantstream-ingest-service-rust/src/route/ingestion.rs",
    "deploy/mock-payer-quantstream-ingest-service-python/app/route/ingestion.py",
    "deploy/mock-payer-quantstream-ingest-service-java/src/io/agentvault/e2e/route/IngestionController.java",
    "deploy/mock-payer-quantstream-ingest-service-typescript/src/route/ingestion.ts",
)

LANGUAGE_IMAGE_COMPONENTS = (
    *(f"mock-payee-marketpulse-{language}-service" for language in (
        "rust", "golang", "java", "python", "typescript",
    )),
    *(f"mock-payer-quantstream-ingest-service-{language}" for language in (
        "rust", "golang", "java", "python", "typescript",
    )),
)

PAYER_FORBIDDEN_SELECTION_TOKENS = (
    "wallet_id",
    "walletId",
    "WalletID",
    "QUANTSTREAM_WALLET_ID",
    "policy_id",
    "policyId",
    "PolicyID",
    "QUANTSTREAM_POLICY_ID",
    "PAYMENT_NETWORK",
    "PAYMENT_ASSET",
    "PAYMENT_TOKEN_CONTRACT",
    "PAYMENT_DECIMALS",
    "PAYMENT_AMOUNT",
    "PAYMENT_RECIPIENT",
    "MARKETPULSE_ORIGIN",
)

FORBIDDEN_RUN_ID_ALIASES = (
    re.compile(r"\bidempotency[_-]?key\b", re.IGNORECASE),
    re.compile(r"\bidempotent[_-]?key\b", re.IGNORECASE),
    re.compile(r"\bexecution[_-]?key\b", re.IGNORECASE),
    re.compile(r"\boperation[_-]?key\b", re.IGNORECASE),
)


def verify(_context: VerificationContext) -> str:
    missing = [relative for relative in REQUIRED if not (E2E_ROOT / relative).is_file()]
    missing.extend(
        f"deploy/{component}/{manifest}"
        for component in DEPLOY_COMPONENTS
        for manifest in COMPONENT_MANIFESTS
        if not (E2E_ROOT / "deploy" / component / manifest).is_file()
    )
    if missing:
        raise RuntimeError("missing E2E files: " + ", ".join(missing))
    forbidden_entry = E2E_ROOT / "deploy" / "e2e.sh"
    if forbidden_entry.exists():
        raise RuntimeError("runner.py must be the only outer E2E orchestrator; delete deploy/e2e.sh")
    outer_python = sorted(path.name for path in E2E_ROOT.glob("*.py"))
    if outer_python != ["runner.py"]:
        raise RuntimeError(f"runner.py must be the only outer Python entry: {outer_python}")
    obsolete = sorted(
        path.relative_to(E2E_ROOT).as_posix()
        for path in (E2E_ROOT / "deploy").rglob("*")
        if path.is_file()
        and not GENERATED_DIRECTORIES.intersection(path.relative_to(E2E_ROOT).parts)
        and (path.name == "k8s-deployment.yaml" or path.suffix == ".yml")
    )
    if obsolete:
        raise RuntimeError("obsolete deployment manifests: " + ", ".join(obsolete))
    compose = (E2E_ROOT / "docker-compose.yaml").read_text(encoding="utf-8")
    if 'user: "65532:65532"' not in compose:
        raise RuntimeError("the read-only evidence auditor must use the business-volume UID")
    for language in ("rust", "go", "python", "typescript", "java"):
        if f":/evidence/quantstream-{language}:ro" not in compose:
            raise RuntimeError(f"the auditor does not mount {language} payer evidence read-only")
        if f":/evidence/marketpulse-{language}:ro" not in compose:
            raise RuntimeError(f"the auditor does not mount {language} payee evidence read-only")
    _verify_sdk_sources()
    _verify_ingestor_sources()
    _verify_ingestion_contracts()
    _verify_payer_selection_boundary()
    _verify_payer_chain_boundary()
    _verify_wallet_pool_contract()
    _verify_run_identity_contract()
    _verify_kubernetes_namespace_boundaries()
    _verify_runtime_image_boundaries()
    manifest_count = len(DEPLOY_COMPONENTS) * len(COMPONENT_MANIFESTS)
    return (
        f"validated {len(REQUIRED) + manifest_count} E2E files, "
        "19 component deployment pairs, the production AgentVault chart, "
        "five strict SDK matrices, and five aligned payer/payee implementations"
    )


def _verify_runtime_image_boundaries() -> None:
    errors: list[str] = []
    for component in LANGUAGE_IMAGE_COMPONENTS:
        dockerfile = E2E_ROOT / "deploy" / component / "Dockerfile"
        stages = [
            line.strip()
            for line in dockerfile.read_text(encoding="utf-8").splitlines()
            if line.strip().startswith("FROM ")
        ]
        if len(stages) != 2 or " AS builder" not in stages[0]:
            errors.append(f"{component} must have exactly one builder and one runtime stage")
        elif "_RUNTIME_IMAGE}" not in stages[1] or "_BUILD_IMAGE}" in stages[1]:
            errors.append(f"{component} runtime stage must not reuse its build image")
    if errors:
        raise RuntimeError("; ".join(errors))


def _verify_kubernetes_namespace_boundaries() -> None:
    namespace_manifest = (E2E_ROOT / "deploy" / "namespaces.yaml").read_text(encoding="utf-8")
    declared = frozenset(re.findall(r"(?m)^  name: (e2e-agentvault-[a-z0-9-]+)$", namespace_manifest))
    if declared != REQUIRED_E2E_NAMESPACES:
        missing = sorted(REQUIRED_E2E_NAMESPACES - declared)
        extra = sorted(declared - REQUIRED_E2E_NAMESPACES)
        raise RuntimeError(f"invalid E2E namespaces: missing={missing}, extra={extra}")

    errors: list[str] = []
    agentvault_values = (E2E_ROOT / "common" / "agentvault-values.yaml").read_text(encoding="utf-8")
    if not re.search(r"(?m)^fullnameOverride: agentvault$", agentvault_values):
        errors.append("AgentVault E2E resources must use the production-style agentvault name")
    for namespace, components in E2E_NAMESPACE_GROUPS.items():
        for component in components:
            chart_root = E2E_ROOT / "deploy" / component / "helm"
            chart = (chart_root / "Chart.yaml").read_text(encoding="utf-8")
            annotation = f"agentvault.io/e2e-namespace: {namespace}"
            if annotation not in chart:
                errors.append(f"{component} must target {namespace}")
            name_match = re.search(r"(?m)^name: ([a-z0-9-]+)$", chart)
            if name_match is None or name_match.group(1).startswith("e2e-"):
                errors.append(f"{component} must use a production-style Chart name")
            template = (chart_root / "templates" / "resources.yaml").read_text(encoding="utf-8")
            if 'index .Chart.Annotations "agentvault.io/e2e-namespace"' not in template:
                errors.append(f"{component} does not source its namespace from the Chart contract")
            if template.count("namespace: {{ $namespace }}") != 4:
                errors.append(f"{component} must namespace its workload, ConfigMap, and Service")
            if 'hasPrefix "e2e-agentvault-" $namespace' not in template:
                errors.append(f"{component} does not reject a non-E2E namespace")
            if "ne .Release.Namespace $namespace" not in template:
                errors.append(f"{component} does not bind Helm release state to its resource namespace")
            if "{{- $name := .Chart.Name -}}" not in template:
                errors.append(f"{component} must not prefix resource names with its E2E release")
    if errors:
        raise RuntimeError("; ".join(errors))


def _verify_sdk_sources() -> None:
    adapters = REPOSITORY_ROOT / "src" / "adapters"
    errors: list[str] = []
    source_text: list[str] = []
    for relative_root, expected in SDK_SOURCE_ALLOWLISTS.items():
        root = adapters / relative_root
        actual = {
            path.relative_to(root).as_posix()
            for path in root.rglob("*")
            if path.is_file()
            and path.suffix in SOURCE_SUFFIXES
            and "tests" not in path.relative_to(root).parts
        }
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        if missing:
            errors.append(f"{relative_root} missing: {', '.join(missing)}")
        if extra:
            errors.append(f"{relative_root} has redundant source: {', '.join(extra)}")
        _verify_test_mirror(adapters, relative_root, root, errors)
        source_text.extend(path.read_text(encoding="utf-8") for path in root.rglob("*")
                           if path.is_file() and path.suffix in SOURCE_SUFFIXES)
    joined = "\n".join(source_text)
    stale = [symbol for symbol in FORBIDDEN_SDK_SYMBOLS if symbol in joined]
    if stale:
        errors.append("forbidden obsolete SDK symbols: " + ", ".join(stale))
    stale_patterns = [pattern for pattern in FORBIDDEN_SDK_PATTERNS if re.search(pattern, joined)]
    if stale_patterns:
        errors.append("forbidden divergent SDK patterns: " + ", ".join(stale_patterns))
    for relative_root, contracts in SDK_MODEL_CONTRACTS.items():
        root = adapters / relative_root
        for relative_file, required_tokens in contracts.items():
            content = (root / relative_file).read_text(encoding="utf-8")
            missing = [token for token in required_tokens if token not in content]
            if missing:
                errors.append(f"{relative_root}/{relative_file} misses DTO parity: {', '.join(missing)}")
    for relative_root, contracts in SDK_LOGICAL_CONTRACTS.items():
        root = adapters / relative_root
        for relative_file, required_tokens in contracts.items():
            source = root / relative_file
            if not source.is_file():
                source = adapters / SDK_TEST_ROOTS[relative_root] / relative_file
            content = source.read_text(encoding="utf-8")
            missing = [token for token in required_tokens if token not in content]
            if missing:
                errors.append(f"{relative_root}/{relative_file} misses logic: {', '.join(missing)}")
    for relative_root, (relative_file, declarations) in MIDDLEWARE_MODEL_DECLARATIONS.items():
        content = (adapters / relative_root / relative_file).read_text(encoding="utf-8")
        duplicated = [declaration for declaration in declarations if declaration in content]
        if duplicated:
            errors.append(f"{relative_root}/{relative_file} redeclares model types: {', '.join(duplicated)}")
    if errors:
        raise RuntimeError("; ".join(errors))


def _verify_test_mirror(adapters, relative_root: str, source_root, errors: list[str]) -> None:
    test_root = adapters / SDK_TEST_ROOTS[relative_root]
    sources = {
        path.relative_to(source_root)
        for path in source_root.rglob("*")
        if path.is_file()
        and path.suffix in SOURCE_SUFFIXES
        and path.name != "__init__.py"
        and test_root not in path.parents
    }
    if relative_root.startswith("java/"):
        expected = {path.with_name(f"{path.stem}Test.java") for path in sources}
    else:
        expected = {path.with_name(f"{path.stem}_test{path.suffix}") for path in sources}
    actual = {
        path.relative_to(test_root)
        for path in test_root.rglob("*")
        if path.is_file() and path.suffix in SOURCE_SUFFIXES and path.name != "__init__.py"
    }
    missing = sorted(path.as_posix() for path in expected - actual)
    extra = sorted(path.as_posix() for path in actual - expected)
    if missing:
        errors.append(f"{relative_root} tests missing: {', '.join(missing)}")
    if extra:
        errors.append(f"{relative_root} has orphan tests: {', '.join(extra)}")


def _verify_ingestion_contracts() -> None:
    for language, required_tokens in JOB_CONTRACTS.items():
        job = (E2E_ROOT / JOB_FILES[language]).read_text(encoding="utf-8")
        missing = [token for token in required_tokens if token not in job]
        if missing:
            raise RuntimeError(f"{language} ingestion job misses: {', '.join(missing)}")
        service = (E2E_ROOT / MCP_FILES[language]).read_text(encoding="utf-8")
        missing = [token for token in MCP_CONTRACTS[language] if token not in service]
        if missing:
            raise RuntimeError(f"{language} MCP helper misses: {', '.join(missing)}")
        missing = [token for token in MCP_TRANSPORT_CONTRACTS[language] if token not in service]
        if missing:
            raise RuntimeError(f"{language} MCP helper misses dual transport evidence: {', '.join(missing)}")
        if DIRECT_TRANSPORT_CONSTRUCTORS[language].search(service):
            raise RuntimeError(f"{language} MCP helper must create transports through the AgentVault facade")
        for tool in ("marketpulse.yesterday_news", "marketpulse.realtime_news"):
            if tool not in service:
                raise RuntimeError(f"{language} MCP service does not register {tool}")
    for relative_file, required_tokens in PAYEE_FRAMEWORK_CONTRACTS.items():
        content = (E2E_ROOT / relative_file).read_text(encoding="utf-8")
        missing = [token for token in required_tokens if token not in content]
        if missing:
            raise RuntimeError(f"{relative_file} misses Payee composition: {', '.join(missing)}")
        language = PAYEE_FRAMEWORK_LANGUAGES[relative_file]
        if DIRECT_TRANSPORT_CONSTRUCTORS[language].search(content):
            raise RuntimeError(f"{relative_file} must create transports through the AgentVault facade")
    for relative_file in PAYEE_ROUTE_FILES + PAYER_ROUTE_FILES:
        content = (E2E_ROOT / relative_file).read_text(encoding="utf-8")
        missing = [path for path in ("/healthz", "/metrics") if path not in content]
        if missing:
            raise RuntimeError(f"{relative_file} misses operational routes: {', '.join(missing)}")
        forbidden = [path for path in ("/mcp", "/api/v1/ingestion") if path in content]
        if forbidden:
            raise RuntimeError(f"{relative_file} owns non-operational routes: {', '.join(forbidden)}")


def _verify_payer_selection_boundary() -> None:
    """Keep wallet/policy inputs behind AgentVault's trusted authorization boundary."""
    violations: list[str] = []
    for root in sorted((E2E_ROOT / "deploy").glob("mock-payer-quantstream-ingest-service-*")):
        for path in root.rglob("*"):
            if not path.is_file() or GENERATED_DIRECTORIES.intersection(path.relative_to(root).parts):
                continue
            try:
                content = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            found = [token for token in PAYER_FORBIDDEN_SELECTION_TOKENS if token in content]
            if found:
                label = path.relative_to(E2E_ROOT).as_posix()
                violations.append(f"{label}: {', '.join(found)}")
    if violations:
        raise RuntimeError(
            "payer services must submit scenario + challenge and let AgentVault select wallets: "
            + "; ".join(violations)
        )


def _verify_payer_chain_boundary() -> None:
    """Payer applications consume SDK results and never own chain connectivity."""
    violations: list[str] = []
    for root in sorted((E2E_ROOT / "deploy").glob("mock-payer-quantstream-ingest-service-*")):
        for path in root.rglob("*"):
            if (
                not path.is_file()
                or path.suffix not in SOURCE_SUFFIXES
                or "tests" in path.relative_to(root).parts
                or GENERATED_DIRECTORIES.intersection(path.relative_to(root).parts)
            ):
                continue
            content = path.read_text(encoding="utf-8")
            found = [pattern.pattern for pattern in PAYER_FORBIDDEN_CHAIN_PATTERNS if pattern.search(content)]
            if found:
                violations.append(f"{path.relative_to(E2E_ROOT).as_posix()}: {', '.join(found)}")
    if violations:
        raise RuntimeError(
            "payer services must not connect to chains; AgentVault owns signing and balance observation: "
            + "; ".join(violations)
        )


def _verify_wallet_pool_contract() -> None:
    """Require five isolated Payer identities with two local wallets each."""
    manifest = json.loads((E2E_ROOT / "common/contract/scenario.json").read_text(encoding="utf-8"))
    wallets = {item.get("id"): item for item in manifest.get("wallets", [])}
    policies = {item.get("id"): item for item in manifest.get("policies", [])}
    scenarios = {item.get("id"): item for item in manifest.get("scenarios", [])}
    clients = {item.get("id"): item for item in manifest.get("clients", [])}
    approvers = {
        (item.get("scenario_id"), item.get("auth_principal"))
        for item in manifest.get("approvers", [])
    }
    expected_wallet_ids: set[str] = set()
    private_key_envs: set[str] = set()
    for identity in PAYER_IDENTITIES.values():
        if identity.client_id not in clients:
            raise RuntimeError(f"missing isolated Payer client: {identity.client_id}")
        scenario = scenarios.get(identity.scenario_id)
        if not isinstance(scenario, dict) or scenario.get("client_id") != identity.client_id:
            raise RuntimeError(f"{identity.scenario_id} must belong only to {identity.client_id}")
        policy = policies.get(identity.policy_id)
        if not isinstance(policy, dict) or policy.get("scenario_id") != identity.scenario_id:
            raise RuntimeError(f"{identity.policy_id} must belong only to {identity.scenario_id}")
        if set(policy.get("wallet_ids", [])) != set(identity.wallet_ids):
            raise RuntimeError(f"{identity.policy_id} must expose exactly its two isolated wallet members")
        if (identity.scenario_id, identity.approver_principal) not in approvers:
            raise RuntimeError(f"missing isolated approver for {identity.scenario_id}")
        for wallet_id, expected_key_env in zip(
            identity.wallet_ids,
            (identity.primary_agentvault_key_env, identity.secondary_agentvault_key_env),
            strict=True,
        ):
            wallet = wallets.get(wallet_id)
            if (
                not isinstance(wallet, dict)
                or wallet.get("custody") != "local"
                or wallet.get("private_key_env") != expected_key_env
            ):
                raise RuntimeError(f"{wallet_id} must be an independently signable local E2E wallet")
            expected_wallet_ids.add(wallet_id)
            private_key_envs.add(expected_key_env)
    if len(expected_wallet_ids) != 10 or len(private_key_envs) != 10:
        raise RuntimeError("the five Payer services must own ten distinct wallets and private-key bindings")
    actual_local_wallets = {
        wallet_id for wallet_id, wallet in wallets.items() if isinstance(wallet, dict) and wallet.get("custody") == "local"
    }
    if actual_local_wallets != expected_wallet_ids:
        raise RuntimeError(
            f"unexpected local E2E wallets: expected={sorted(expected_wallet_ids)} actual={sorted(actual_local_wallets)}"
        )


def _verify_run_identity_contract() -> None:
    """Keep `run_id` as the only durable business retry and deduplication key."""
    roots = (REPOSITORY_ROOT / "src" / "adapters", E2E_ROOT / "deploy")
    violations: list[str] = []
    allowed_suffixes = SOURCE_SUFFIXES | {".json", ".mjs", ".sh", ".yaml"}
    for root in roots:
        for path in root.rglob("*"):
            if (
                not path.is_file()
                or path.suffix not in allowed_suffixes
                or GENERATED_DIRECTORIES.intersection(path.relative_to(root).parts)
            ):
                continue
            try:
                content = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            found = [pattern.pattern for pattern in FORBIDDEN_RUN_ID_ALIASES if pattern.search(content)]
            if found:
                violations.append(f"{path.relative_to(REPOSITORY_ROOT).as_posix()}: {', '.join(found)}")
    if violations:
        raise RuntimeError(
            "run_id is the only business idempotency identity; remove alternate key fields: "
            + "; ".join(violations)
        )
    mqtt_schema = json.loads(
        (E2E_ROOT / "common/contract/payment-mqtt.schema.json").read_text(encoding="utf-8")
    )
    approval = mqtt_schema.get("$defs", {}).get("approvalRequired", {})
    if "runId" not in approval.get("required", []) or "runId" not in approval.get("properties", {}):
        raise RuntimeError("MQTT approval_required must carry the canonical business runId")


def _verify_ingestor_sources() -> None:
    errors: list[str] = []
    for relative_root, expected in INGESTOR_SOURCE_ALLOWLISTS.items():
        root = E2E_ROOT / relative_root
        actual = {
            path.relative_to(root).as_posix()
            for path in root.rglob("*")
            if path.is_file()
            and path.suffix in SOURCE_SUFFIXES
            and "tests" not in path.relative_to(root).parts
        }
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        if missing:
            errors.append(f"{relative_root} missing: {', '.join(missing)}")
        if extra:
            errors.append(f"{relative_root} has redundant source: {', '.join(extra)}")
        _verify_component_test_mirror(
            root,
            E2E_ROOT / INGESTOR_TEST_ROOTS[relative_root],
            relative_root.endswith("/e2e"),
            errors,
            relative_root,
        )
    for relative_root, expected in PAYEE_SOURCE_ALLOWLISTS.items():
        root = E2E_ROOT / relative_root
        actual = {
            path.relative_to(root).as_posix()
            for path in root.rglob("*")
            if path.is_file()
            and path.suffix in SOURCE_SUFFIXES
            and "tests" not in path.relative_to(root).parts
        }
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        if missing:
            errors.append(f"{relative_root} missing: {', '.join(missing)}")
        if extra:
            errors.append(f"{relative_root} has redundant source: {', '.join(extra)}")
        _verify_component_test_mirror(
            root,
            E2E_ROOT / PAYEE_TEST_ROOTS[relative_root],
            relative_root.endswith("/e2e"),
            errors,
            relative_root,
        )
    if errors:
        raise RuntimeError("; ".join(errors))


def _verify_component_test_mirror(source_root, test_root, java: bool, errors: list[str], label: str) -> None:
    sources = {
        path.relative_to(source_root)
        for path in source_root.rglob("*")
        if path.is_file()
        and path.suffix in SOURCE_SUFFIXES
        and path.name != "__init__.py"
        and test_root not in path.parents
    }
    if java:
        expected = {path.with_name(f"{path.stem}Test.java") for path in sources}
    else:
        expected = {path.with_name(f"{path.stem}_test{path.suffix}") for path in sources}
    actual = {
        path.relative_to(test_root)
        for path in test_root.rglob("*")
        if path.is_file() and path.suffix in SOURCE_SUFFIXES and path.name != "__init__.py"
    }
    missing = sorted(path.as_posix() for path in expected - actual)
    extra = sorted(path.as_posix() for path in actual - expected)
    if missing:
        errors.append(f"{label} tests missing: {', '.join(missing)}")
    if extra:
        errors.append(f"{label} has orphan tests: {', '.join(extra)}")
