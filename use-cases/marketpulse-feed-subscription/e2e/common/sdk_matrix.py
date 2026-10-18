from __future__ import annotations

from dataclasses import dataclass

from common.payer_identity import LANGUAGES


SDK_SCOPES = ("simple", "full")
SIMPLE_LANGUAGES = ("rust", "go")
TRANSPORTS = ("mqtt", "grpc")


@dataclass(frozen=True)
class SdkMatrixCase:
    payer: str
    payee: str
    transport: str

    @property
    def name(self) -> str:
        return f"payer={self.payer} payee={self.payee} transport={self.transport}"


def matrix_languages(scope: str) -> tuple[str, ...]:
    if scope == "simple":
        return SIMPLE_LANGUAGES
    if scope == "full":
        return LANGUAGES
    raise ValueError(f"unsupported E2E SDK scope: {scope}")


def matrix_cases(scope: str) -> tuple[SdkMatrixCase, ...]:
    languages = matrix_languages(scope)
    return tuple(
        SdkMatrixCase(payer=payer, payee=payee, transport=transport)
        for transport in TRANSPORTS
        for payer in languages
        for payee in languages
    )


def matrix_case_keys(scope: str) -> set[tuple[str, str, str]]:
    return {(case.payer, case.payee, case.transport) for case in matrix_cases(scope)}
