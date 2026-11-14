from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import yaml
from util.constant import E2EOtherDefinitions as Project

LANGUAGES = Project.LANGUAGES


@dataclass(frozen=True)
class PayerIdentity:
    language: str
    client_id: str
    scenario_id: str
    policy_id: str
    approver_principal: str
    primary_wallet_id: str
    secondary_wallet_id: str
    primary_private_key_env: str
    secondary_private_key_env: str
    primary_agentvault_key_env: str
    secondary_agentvault_key_env: str

    @property
    def wallet_ids(self) -> tuple[str, str]:
        return self.primary_wallet_id, self.secondary_wallet_id

    @classmethod
    def _create(cls, language: str) -> PayerIdentity:
        upper = language.upper()
        prefix = f"quantstream-{language}"
        return cls(
            language=language,
            client_id=f"client-{prefix}",
            scenario_id=f"scenario-{prefix}-market-ingestion",
            policy_id=f"policy-{prefix}-market-data",
            approver_principal=f"principal-{prefix}-admin",
            primary_wallet_id=f"wallet-{prefix}-primary",
            secondary_wallet_id=f"wallet-{prefix}-secondary",
            primary_private_key_env=f"E2E_QUANTSTREAM_{upper}_PRIMARY_PAYER_PRIVATE_KEY",
            secondary_private_key_env=f"E2E_QUANTSTREAM_{upper}_SECONDARY_PAYER_PRIVATE_KEY",
            primary_agentvault_key_env=f"HELPER_SECRET_QUANTSTREAM_{upper}_PRIMARY_WALLET_PRIVATE_KEY",
            secondary_agentvault_key_env=f"HELPER_SECRET_QUANTSTREAM_{upper}_SECONDARY_WALLET_PRIVATE_KEY",
        )

    @staticmethod
    def _lookup(language: str) -> PayerIdentity:
        try:
            return PAYER_IDENTITIES[language]
        except KeyError as error:
            raise ValueError(f"unsupported payer language: {language}") from error


PAYER_IDENTITIES = {language: PayerIdentity._create(language) for language in LANGUAGES}


def payer_identity(language: str) -> PayerIdentity:
    return PayerIdentity._lookup(language)


COLLECTION_BY_KIND = {
    "Client": "clients",
    "Wallet": "wallets",
    "Scenario": "scenarios",
    "Policy": "policies",
    "Price": "prices",
    "Approver": "approvers",
}

_CANONICAL_MARKETPULSE_ORIGIN = "http://127.0.0.1:8081"
_MARKETPULSE_PRICE = "price-realtime-news"


class ConsoleResourceFixture:
    @staticmethod
    def _resource_documents(path: Path) -> list[dict[str, Any]]:
        documents: list[dict[str, Any]] = []
        try:
            parsed_documents = yaml.safe_load_all(path.read_text(encoding="utf-8"))
            for index, document in enumerate(parsed_documents, start=1):
                if document is None:
                    continue
                if not isinstance(document, dict):
                    raise ValueError(
                        f"resource document {index} must be a YAML mapping"
                    )
                if set(document) != {"consoleVersion", "kind", "metadata", "data"}:
                    raise ValueError(
                        f"resource document {index} has an invalid envelope"
                    )
                if document["consoleVersion"] != "core.agentvault.io/v1":
                    raise ValueError(
                        f"resource document {index} has an unsupported consoleVersion"
                    )
                kind = document["kind"]
                if kind not in COLLECTION_BY_KIND:
                    raise ValueError(
                        f"resource document {index} has an unsupported kind: {kind}"
                    )
                metadata = document["metadata"]
                data = document["data"]
                if not isinstance(metadata, dict) or set(metadata) != {
                    "name",
                    "status",
                }:
                    raise ValueError(f"resource document {index} has invalid metadata")
                if not isinstance(data, dict):
                    raise ValueError(f"resource document {index} has invalid data")
                documents.append(document)
        except yaml.YAMLError as error:
            raise ValueError(f"invalid resource YAML: {error}") from error
        if not documents:
            raise ValueError("resource fixture is empty")
        return documents

    @staticmethod
    def _normalized_origin(origin: str) -> str:
        parsed = urlsplit(origin)
        if (
            parsed.scheme not in {"http", "https"}
            or not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.query
            or parsed.fragment
            or parsed.path not in {"", "/"}
        ):
            raise ValueError(
                "MarketPulse origin must be an absolute HTTP(S) origin without credentials or a path"
            )
        return urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))

    @staticmethod
    def render_agentvault_resources(path: Path, marketpulse_origin: str) -> str:
        """Render the canonical import fixture for one runtime origin.

        Environment projection is intentionally limited to the origin-bound Policy
        and Price fields.  All authoritative identifiers and payment constraints
        remain byte-for-byte equivalent to the canonical fixture.
        """
        origin = ConsoleResourceFixture._normalized_origin(marketpulse_origin)
        documents = ConsoleResourceFixture._resource_documents(path)
        policy_count = 0
        price_count = 0
        for document in documents:
            data = document["data"]
            if document["kind"] == "Policy":
                if data.get("allowed_origins") != [_CANONICAL_MARKETPULSE_ORIGIN]:
                    raise ValueError(
                        "canonical Policy allowed_origins drifted from the expected E2E origin"
                    )
                data["allowed_origins"] = [origin]
                policy_count += 1
            elif (
                document["kind"] == "Price"
                and document["metadata"]["name"] == _MARKETPULSE_PRICE
            ):
                if data.get("resource") != f"{_CANONICAL_MARKETPULSE_ORIGIN}/mcp":
                    raise ValueError(
                        "canonical MarketPulse Price resource drifted from the expected E2E URL"
                    )
                data["resource"] = f"{origin}/mcp"
                price_count += 1
        if policy_count == 0 or price_count != 1:
            raise ValueError(
                "canonical fixture must contain Policies and exactly one MarketPulse Price"
            )
        return yaml.safe_dump_all(
            documents,
            allow_unicode=True,
            default_flow_style=False,
            explicit_start=True,
            sort_keys=False,
        )

    @staticmethod
    def load_agentvault_resources(path: Path) -> dict[str, list[dict[str, Any]]]:
        """Load strict resource documents and expose their domain data by kind."""
        collections = {name: [] for name in COLLECTION_BY_KIND.values()}
        for document in ConsoleResourceFixture._resource_documents(path):
            kind = document["kind"]
            collection = COLLECTION_BY_KIND[kind]
            metadata = document["metadata"]
            data = document["data"]
            resource = dict(data)
            if kind != "Approver":
                resource["id"] = metadata["name"]
            resource["enabled"] = metadata["status"] == "active"
            collections[collection].append(resource)
        return collections


@dataclass(frozen=True)
class VerificationContext:
    run_id: str
    report_root: Path
    deploy_root: Path
    scenario: str
    round_number: int
    payer_language: str
    payee_language: str
    transport: str
    deployer: str
    timeout_seconds: float
    skip_build: bool
    reuse_build: bool
    e2e_sdk_scope: str = "simple"
    matrix_workers: int = 2
    worker_slot: int | None = None
    evidence_run_id: str | None = None
    payment_profile: str = "evm"
    message_protocol: str = "mcp"
    fail_after_settlement: bool = False
    state: dict = field(default_factory=dict, compare=False, repr=False)
    completed: dict = field(default_factory=dict, compare=False, repr=False)

    def require(self, scenario: str):
        if scenario not in self.completed:
            raise RuntimeError(
                f"scenario requires successful {scenario} evidence from this run"
            )
        return self.completed[scenario]

    def environment(self) -> dict[str, str]:
        payer = payer_identity(self.payer_language)
        return {
            "E2E_RUNNER_RUN_ID": self.run_id,
            "E2E_RUNNER_DEPLOYER": self.deployer,
            "E2E_RUNNER_SKIP_BUILD": "1" if self.skip_build else "0",
            "E2E_RUNNER_REUSE_BUILD": "1" if self.reuse_build else "0",
            "E2E_MATRIX_WORKERS": str(self.matrix_workers),
            "E2E_SDK_SCOPE": self.e2e_sdk_scope,
            "E2E_WORKER_SLOT": ""
            if self.worker_slot is None
            else str(self.worker_slot),
            "E2E_RUNNER_PAYER_LANGUAGE": self.payer_language,
            "E2E_RUNNER_PAYEE_LANGUAGE": self.payee_language,
            "E2E_AGENTVAULT_TRANSPORT": self.transport,
            "E2E_AUDITOR_PAYER_LANGUAGE": self.payer_language,
            "E2E_AUDITOR_PAYEE_LANGUAGE": self.payee_language,
            "E2E_QUANTSTREAM_CLIENT_ID": payer.client_id,
            "E2E_QUANTSTREAM_SCENARIO_ID": payer.scenario_id,
            "E2E_QUANTSTREAM_UI_APPROVER_PRINCIPAL": payer.approver_principal,
            # The runner is the lifecycle owner. It retains the stack unless
            # --cleanup-after-run is explicitly requested.
            "E2E_RUNNER_KEEP_STACK": "1",
        }


@dataclass(frozen=True)
class FullFlowEvidence:
    payer_language: str
    payee_language: str
    transport: str
    audit: dict[str, Any]
    initial_balances: dict[str, int]
    final_balances: dict[str, int]
    chain_accounting: dict[str, Any]
    logs: dict[str, str]
    runtime: dict[str, str]


class E2EDeployer(ABC):
    """Backend-neutral lifecycle used by every E2E verifier."""

    backend = ""

    def __init__(self, context: VerificationContext) -> None:
        self.context = context

    @abstractmethod
    def render(self) -> str:
        """Validate and render the complete deployment."""

    @abstractmethod
    def smoke(self, role: str) -> str:
        """Start and verify dependencies for one payment role."""

    @abstractmethod
    def full(
        self,
        payer_language: str,
        payee_language: str,
        *,
        clean_start: bool = True,
        render_config: bool = True,
    ) -> dict[str, str]:
        """Run one payer/payee language pair through the complete payment flow."""

    @abstractmethod
    def matrix(self) -> dict[str, str]:
        """Run every supported language through the payment flow."""

    @abstractmethod
    def mpp_tempo(self) -> str:
        """Run the real MPP Tempo charge and recovery gate."""

    @abstractmethod
    def x402_solana(self) -> str:
        """Run the real x402 v2 Solana exact payment gate."""

    @abstractmethod
    def cleanup(self) -> str:
        """Remove only resources owned by this E2E deployment."""

    @abstractmethod
    def cleanup_images(self) -> str:
        """Remove only locally built images owned by this E2E suite."""


def create_deployer(context: VerificationContext) -> E2EDeployer:
    """Resolve case-specific orchestration lazily, keeping utilities independent."""
    if "runtime" in context.state:
        return context.state["runtime"]
    if context.deployer == "docker":
        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

        runtime = E2ES20PurchaseVerifier.DockerRuntime(context)
    elif context.deployer == "kubernetes":
        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

        runtime = E2ES20PurchaseVerifier.KubernetesRuntime(context)
    else:
        raise ValueError(f"unsupported E2E deployer: {context.deployer}")
    context.state["runtime"] = runtime
    return runtime
