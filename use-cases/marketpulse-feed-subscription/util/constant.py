from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


class E2EOtherDefinitions:
    @dataclass(frozen=True)
    class SdkMatrixCase:
        payer: str
        payee: str
        transport: str

        @property
        def name(self) -> str:
            return f"payer={self.payer} payee={self.payee} transport={self.transport}"

    E2E_ROOT = Path(__file__).resolve().parents[1]
    REPOSITORY_ROOT = E2E_ROOT.parents[1]
    AGENTVAULT_ROOT = E2E_ROOT / "deploy" / "agentvault"
    AGENTVAULT_CONFIG = AGENTVAULT_ROOT / "config"
    AGENTVAULT_CHART = REPOSITORY_ROOT / "deploy/helm/agentvault"

    DEPLOY_ROOT = E2E_ROOT / "deploy"
    REPORT_ROOT = E2E_ROOT / "reports"

    DEPLOY_COMPONENTS = (
        "agentvault",
        "anvil",
        "evm",
        "facilitator",
        "mock-mpp-tempo-service",
        "jaeger",
        "mock-payee-marketpulse-golang-service",
        "mock-payee-marketpulse-java-service",
        "mock-payee-marketpulse-python-service",
        "mock-payee-marketpulse-rust-service",
        "mock-payee-marketpulse-typescript-service",
        "mock-payer-quantstream-service-golang",
        "mock-payer-quantstream-service-java",
        "mock-payer-quantstream-service-python",
        "mock-payer-quantstream-service-rust",
        "mock-payer-quantstream-service-typescript",
        "mock-payer-quantstream-ui",
        "postgres",
        "solana",
        "tempo",
    )
    # AgentVault reuses its production chart; other components own local charts.
    COMPONENTS_WITH_LOCAL_CHART = tuple(
        name for name in DEPLOY_COMPONENTS if name != "agentvault"
    )

    LANGUAGES = ("rust", "go", "python", "typescript", "java")
    SDK_SCOPES = ("simple", "full")
    SIMPLE_LANGUAGES = ("rust", "go")
    TRANSPORTS = ("http",)

    @classmethod
    def component_chart(cls, component: str) -> Path:
        return (
            cls.AGENTVAULT_CHART
            if component == "agentvault"
            else cls.DEPLOY_ROOT / component / "helm"
        )

    @classmethod
    def matrix_languages(cls, scope: str) -> tuple[str, ...]:
        if scope == "simple":
            return cls.SIMPLE_LANGUAGES
        if scope == "full":
            return cls.LANGUAGES
        raise ValueError(f"unsupported E2E SDK scope: {scope}")

    @classmethod
    def matrix_cases(cls, scope: str) -> tuple[E2EOtherDefinitions.SdkMatrixCase, ...]:
        languages = cls.matrix_languages(scope)
        return tuple(
            E2EOtherDefinitions.SdkMatrixCase(
                payer=payer, payee=payee, transport=transport
            )
            for transport in cls.TRANSPORTS
            for payer in languages
            for payee in languages
        )

    @classmethod
    def matrix_case_keys(cls, scope: str) -> set[tuple[str, str, str]]:
        return {
            (case.payer, case.payee, case.transport) for case in cls.matrix_cases(scope)
        }
