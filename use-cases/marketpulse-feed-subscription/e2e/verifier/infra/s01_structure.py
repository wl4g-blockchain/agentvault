from __future__ import annotations

import re

from common.context import VerificationContext
from common.project import E2E_ROOT, REPOSITORY_ROOT


REQUIRED = (
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
)

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
        "payee/gin.go", "transport/http.go", "transport/grpc.go", "transport/mqtt.go",
    },
    "rust/src": {
        "lib.rs", "agentvault.rs", "config/config.rs",
        "model/approval.rs", "model/base.rs", "model/payee.rs", "model/payer.rs",
        "payer/payer.rs", "payer/standard.rs", "payee/payee.rs", "payee/standard.rs",
        "payee/axum.rs", "transport/http.rs", "transport/grpc.rs", "transport/mqtt.rs",
    },
    "python/agentvault": {
        "__init__.py", "agentvault.py", "config/__init__.py", "config/config.py",
        "model/__init__.py", "model/approval.py", "model/base.py", "model/payee.py", "model/payer.py",
        "payer/__init__.py", "payer/payer.py", "payer/standard.py", "payee/__init__.py",
        "payee/payee.py", "payee/standard.py", "payee/fast.py", "transport/__init__.py",
        "transport/http.py", "transport/grpc.py", "transport/mqtt.py",
    },
    "typescript/src/agentvault": {
        "agentvault.ts", "config/config.ts", "model/approval.ts", "model/base.ts",
        "model/payee.ts", "model/payer.ts", "payer/payer.ts", "payer/standard.ts",
        "payee/payee.ts", "payee/standard.ts", "payee/express.ts", "transport/http.ts",
        "transport/grpc.ts", "transport/mqtt.ts",
    },
    "java/src/io/agentvault/sdk": {
        "AgentVault.java", "config/Config.java", "model/Approval.java",
        "model/Base.java", "model/Payee.java", "model/Payer.java", "payer/PayerMiddleware.java",
        "payer/StandardPayerMiddleware.java", "payee/PayeeMiddleware.java",
        "payee/StandardPayeeMiddleware.java", "payee/SpringInterceptorPayeeMiddleware.java",
        "transport/HttpVaultTransport.java", "transport/GrpcVaultTransport.java",
        "transport/MqttVaultTransport.java",
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
                           "type SettlePaymentRequest", "type IncomingPayment", "type SettledIncomingPayment",
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
                           "pub struct IncomingPayment", "pub struct SettledIncomingPayment",
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
                           "class SettlePaymentRequest", "class IncomingPayment", "class SettledIncomingPayment",
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
                           "interface ReportOutboundResponseRequest", "interface ReportOutboundResponseResponse",
                           "interface BalanceQuery", "interface PaymentBalance", "class PayerRequestContext",
                           "interface PayerHttpRequest", "interface PayerHttpResponse",
                           "interface AuthorizedPayment"),
        "model/payee.ts": ("interface ResolveOfferRequest", "interface PaymentOffer", "interface PayeeOperation",
                           "interface CreateChallengeRequest", "interface VerifyPaymentRequest",
                           "interface PaymentVerification", "interface SettlePaymentRequest",
                           "interface IncomingPayment", "interface SettledIncomingPayment",
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
                             "record PaymentVerification", "record SettlePaymentRequest", "record IncomingPayment",
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
                            "protocolHint != \"\" && protocolHint != m.config.Protocol", "intent.ScenarioID != m.config.ScenarioID",
                            "intent.Asset != m.config.Asset", "intent.IdempotencyKey != string(operation.IdempotencyKey)"),
        "payer/standard.go": ("type StandardPayerMiddleware", "RoundTrip(", "DeliveryRetries", "StatusPaymentRequired", "StatusServiceUnavailable", "CheckRedirect"),
        "payer/payer_test.go": ("RejectsProtocolHintMismatchBeforeVaultCall", "payment-protocol", "mpp"),
        "payee/payee.go": ("type PayeeMiddleware", "ErrInvalidPayment", "Challenge(", "Protect(", "ResolveOffer(", "GetReceipt(",
                            "ExecutionKey:     operation.ExecutionKey",
                            "intent.Asset != challenge.Asset", "payment.Intent.Status != model.PaymentStatusSettled",
                            "payment.Receipt.Status != model.PaymentStatusSettled"),
        "payee/gin.go": ("type GinPayeeMiddleware", "Handler() gin.HandlerFunc", "NewMCPToolRoute", "isMCPToolCall", "errors.Is(err, ErrInvalidPayment)"),
        "payee/standard.go": ("type StandardPayeeMiddleware", "NewStandardMCPToolRoute", "isMCPToolCall", "errors.Is(err, ErrInvalidPayment)"),
        "payee/payee_test.go": ("RejectsInvalidPaymentBeforeSettlement", "/api/v1/payee/verify", "ErrInvalidPayment"),
        "transport/http.go": ("type HttpVaultTransport",),
        "transport/grpc.go": ("type GrpcVaultTransport",),
        "transport/mqtt.go": ("type MqttVaultTransport", "MaxResponseBytes"),
    },
    "rust/src": {
        "agentvault.rs": ("pub struct AgentVault", "pub fn payer", "pub fn payee", "get_pending_approvals", "submit_approval_decision"),
        "model/payer.rs": ("pub struct PayerRequestContext", "pub struct PayerHttpRequest", "pub struct PayerHttpResponse", "pub struct AuthorizedPayment", "pub struct BalanceQuery"),
        "model/payee.rs": ("pub struct PayeeOperation", "pub struct ResolveOfferRequest", "pub enum ProtectionOutcome"),
        "payer/payer.rs": ("pub struct PayerMiddleware", "pub async fn authorize", "pub async fn report", "pub fn credential_header", "pub async fn get_payment", "pub async fn get_receipt", "pub async fn get_balance",
                            "validate_protocol_hint", "intent.scenario_id != config.scenario_id",
                            "intent.asset != config.asset", "intent.idempotency_key != context.idempotency_key.as_str()"),
        "payer/standard.rs": ("pub struct ReqwestPayerMiddleware", "impl<T> Middleware", "delivery_retries", "Policy::none", "status != 503"),
        "payer/payer_test.rs": ("payer_rejects_protocol_hint_mismatch_before_vault_call", "payment-protocol", "mpp"),
        "payee/payee.rs": ("pub struct PayeeMiddleware", "pub async fn challenge", "pub async fn protect", "pub async fn resolve_offer", "pub async fn get_receipt",
                            "execution_key: operation.execution_key.clone()",
                            "intent.asset != challenge.asset", "payment.intent.status != crate::PaymentStatus::Settled",
                            "payment.receipt.status != crate::PaymentStatus::Settled"),
        "payee/axum.rs": ("pub struct AxumPayeeMiddleware", "pub fn mcp_tool", "is_mcp_tool_call"),
        "payee/payee_test.rs": ("payee_rejects_invalid_payment_before_settlement", "/api/v1/payee/verify", "status: 402"),
        "transport/http.rs": ("pub struct HttpVaultTransport",),
        "transport/grpc.rs": ("pub struct GrpcVaultTransport",),
        "transport/mqtt.rs": ("pub struct MqttVaultTransport", "max_response_bytes"),
    },
    "python/agentvault": {
        "agentvault.py": ("class AgentVault", "def payer", "def payee", "def get_pending_approvals", "def submit_approval_decision"),
        "model/payer.py": ("class PayerRequestContext", "class PayerHttpRequest", "class PayerHttpResponse", "class AuthorizedPayment", "class BalanceQuery"),
        "model/payee.py": ("class PayeeOperation", "class ResolveOfferRequest"),
        "payer/payer.py": ("class PayerMiddleware", "def authorize", "def report", "def credential_header", "def get_payment", "def get_receipt", "def get_balance",
                            "required.protocol_hint != self.config.protocol", "intent.scenario_id != self.config.scenario_id",
                            "intent.asset != self.config.asset", "intent.idempotency_key != operation.idempotency_key"),
        "payer/standard.py": ("class HttpxPayerMiddleware", "async def request", "delivery_retries", "follow_redirects=False", "stream=True", "_read_bounded", "response.status_code != 503"),
        "payer/payer_test.py": ("test_rejects_protocol_hint_mismatch_before_vault_call", "payment-protocol", "mpp"),
        "payee/payee.py": ("class PayeeMiddleware", "def challenge", "def protect", "def resolve_offer", "def get_receipt",
                            "operation.settlement_context(), operation.execution_key",
                            "intent.asset != challenge.asset", "intent.status.value != \"settled\"",
                            "receipt.status.value != \"settled\""),
        "payee/fast.py": ("class FastPayeeMiddleware", "async def __call__", "def mcp_tool", "_is_mcp_tool_call"),
        "payee/payee_test.py": ("test_rejects_invalid_payment_before_settlement", "/api/v1/payee/verify", "PermissionError"),
        "transport/http.py": ("class HttpVaultTransport",),
        "transport/grpc.py": ("class GrpcVaultTransport",),
        "transport/mqtt.py": ("class MqttVaultTransport", "max_response_bytes"),
    },
    "typescript/src/agentvault": {
        "agentvault.ts": ("class AgentVault", "payer(", "payee(", "getPendingApprovals", "submitApprovalDecision"),
        "model/payer.ts": ("class PayerRequestContext", "interface PayerHttpRequest", "interface PayerHttpResponse", "interface AuthorizedPayment", "interface BalanceQuery"),
        "model/payee.ts": ("interface PayeeOperation", "interface ResolveOfferRequest", "type ProtectionOutcome"),
        "payer/payer.ts": ("class PayerMiddleware", "async authorize", "async report", "credentialHeader", "getPayment", "getReceipt", "getBalance",
                            "protocolHint !== this.config.protocol", "intent.scenario_id !== this.config.scenarioId",
                            "!sameAsset(intent.asset, this.config.asset)", "intent.idempotency_key !== operation.idempotencyKey"),
        "payer/standard.ts": ("class FetchPayerMiddleware", "async request", "deliveryRetries", "redirect: 'manual'", "response.status !== 503"),
        "payer/payer_test.ts": ("rejects a protocol hint mismatch before calling AgentVault", "payment-protocol", "mpp"),
        "payee/payee.ts": ("class PayeeMiddleware", "class InvalidPaymentError", "async challenge", "async protect", "resolveOffer", "getReceipt",
                            "execution_key: operation.executionKey",
                            "!sameAsset(intent.asset, challenge.asset)", "intent.status !== 'settled'",
                            "receipt.status !== 'settled'"),
        "payee/express.ts": ("class ExpressPayeeMiddleware", "handler()", "mcpToolRoute", "isMcpToolCall", "error instanceof InvalidPaymentError"),
        "payee/payee_test.ts": ("rejects invalid payment before settlement", "/api/v1/payee/verify", "InvalidPaymentError"),
        "transport/http.ts": ("class HttpVaultTransport",),
        "transport/grpc.ts": ("class GrpcVaultTransport",),
        "transport/mqtt.ts": ("class MqttVaultTransport", "maxResponseBytes"),
    },
    "java/src/io/agentvault/sdk": {
        "AgentVault.java": ("class AgentVault", "payer(", "payee(", "getPendingApprovals", "submitApprovalDecision"),
        "model/Payer.java": ("record PayerRequestContext", "record PayerHttpRequest", "record PayerHttpResponse", "record AuthorizedPayment", "record BalanceQuery"),
        "model/Payee.java": ("record PayeeOperation", "record ResolveOfferRequest", "interface ProtectionOutcome"),
        "payer/PayerMiddleware.java": ("class PayerMiddleware", "AuthorizedPayment authorize", "void report", "credentialHeader", "getPayment", "getReceipt", "getBalance",
                            "!challenge.protocolHint().equals(config.protocol())", "!config.scenarioId().equals(intent.scenarioId())",
                            "!config.asset().equals(intent.asset())", "!context.idempotencyKey().equals(intent.idempotencyKey())"),
        "payer/StandardPayerMiddleware.java": ("class StandardPayerMiddleware", "intercept(", "deliveryRetries", "HttpClient.Redirect.NEVER", "value() != 402", "value() != 503"),
        "payer/PayerMiddlewareTest.java": ("rejectsProtocolHintMismatchBeforeCallingAgentVault", "payment-protocol", "mpp"),
        "payee/PayeeMiddleware.java": ("class PayeeMiddleware", "challenge(", "protect(", "resolveOffer", "getReceipt",
                            "new SettlePaymentRequest(settlement, operation.executionKey()",
                            "!challenge.asset().equals(intent.asset())", "payment.intent().status() != Base.PaymentStatus.SETTLED",
                            "payment.receipt().status() != Base.PaymentStatus.SETTLED"),
        "payee/SpringInterceptorPayeeMiddleware.java": ("class SpringInterceptorPayeeMiddleware", "doFilterInternal", "mcpTool", "isMcpToolCall"),
        "payee/PayeeMiddlewareTest.java": ("rejectsInvalidPaymentBeforeSettlement", "/api/v1/payee/verify", "SecurityException"),
        "transport/HttpVaultTransport.java": ("class HttpVaultTransport",),
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

PAYEE_FRAMEWORK_CONTRACTS = {
    "deploy/mock-payee-marketpulse-golang-service/pkg/server.go": (
        "GinPayeeMiddleware", "NewFeedSQLiteStorage", "NewFeedHandler", "NewFeedRouter", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-rust-service/src/server.rs": (
        "AxumPayeeMiddleware", "FeedSQLiteStorage", "FeedHandler", "FeedRouter", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-python-service/app/server.py": (
        "FastAPI", "FastPayeeMiddleware", "FeedSQLiteStorage", "FeedHandler", "FeedRouter", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-java-service/src/io/agentvault/e2e/server/MarketPulseServer.java": (
        "MarketPulseServer", "SpringInterceptorPayeeMiddleware", "FeedSQLiteStorage", "McpEndpoint", '"/mcp"',
    ),
    "deploy/mock-payee-marketpulse-typescript-service/src/server.ts": (
        "ExpressPayeeMiddleware", "FeedSQLiteStorage", "FeedHandler", "FeedRouter", "'/mcp'",
    ),
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
    manifest_count = len(DEPLOY_COMPONENTS) * len(COMPONENT_MANIFESTS)
    return (
        f"validated {len(REQUIRED) + manifest_count} E2E files, "
        "18 component deployment pairs, the production AgentVault chart, "
        "five strict SDK matrices, and five aligned payer/payee implementations"
    )


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
        for tool in ("marketpulse.yesterday_news", "marketpulse.realtime_news"):
            if tool not in service:
                raise RuntimeError(f"{language} MCP service does not register {tool}")
    for relative_file, required_tokens in PAYEE_FRAMEWORK_CONTRACTS.items():
        content = (E2E_ROOT / relative_file).read_text(encoding="utf-8")
        missing = [token for token in required_tokens if token not in content]
        if missing:
            raise RuntimeError(f"{relative_file} misses Payee composition: {', '.join(missing)}")
    for relative_file in PAYEE_ROUTE_FILES + PAYER_ROUTE_FILES:
        content = (E2E_ROOT / relative_file).read_text(encoding="utf-8")
        missing = [path for path in ("/healthz", "/metrics") if path not in content]
        if missing:
            raise RuntimeError(f"{relative_file} misses operational routes: {', '.join(missing)}")
        forbidden = [path for path in ("/mcp", "/api/v1/ingestion") if path in content]
        if forbidden:
            raise RuntimeError(f"{relative_file} owns non-operational routes: {', '.join(forbidden)}")


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
