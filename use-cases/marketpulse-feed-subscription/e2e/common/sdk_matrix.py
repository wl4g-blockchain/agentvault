from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path

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


class SdkMatrixJournal:
    """Atomic matrix checkpoints bound to one immutable scope and deployment backend."""

    REPORTS = frozenset({
        "s20_full.md", "s21_payer_signing.md", "s22_payee_settlement.md",
        "s23_balance_sync.md", "s24_gateway_balance.md",
    })

    def __init__(self, root: Path, scope: str, backend: str) -> None:
        if backend not in {"docker", "kubernetes"}:
            raise ValueError(f"unsupported matrix backend: {backend}")
        self.root = root
        self.backend = backend
        self._cases = tuple(case.name for case in matrix_cases(scope))
        plan = {
            "backend": backend,
            "scope": scope,
            "languages": list(matrix_languages(scope)),
            "transports": list(TRANSPORTS),
            "cases": list(self._cases),
        }
        target = root / "matrix-plan.json"
        if target.is_file():
            if self._read(target) != plan:
                raise RuntimeError(f"matrix scope/backend does not match the immutable resume plan: {target}")
        else:
            self._write(target, plan)

    def load(self, case_number: int, name: str) -> dict[str, str] | None:
        target = self._target(case_number, name)
        if not target.is_file():
            return None
        payload = self._read(target)
        if not isinstance(payload, dict) or payload.get("backend") != self.backend or payload.get("case") != name:
            raise RuntimeError(f"matrix checkpoint backend/identity mismatch: {target}")
        return self._reports(payload.get("reports"), target)

    def save(self, case_number: int, name: str, outputs: dict[str, str]) -> None:
        target = self._target(case_number, name)
        self._write(target, {"backend": self.backend, "case": name, "reports": self._reports(outputs, target)})

    def _target(self, case_number: int, name: str) -> Path:
        if not 1 <= case_number <= len(self._cases) or self._cases[case_number - 1] != name:
            raise ValueError("matrix checkpoint does not identify a case in the selected scope")
        return self.root / "matrix-cases" / f"case-{case_number:02d}.json"

    @classmethod
    def _reports(cls, outputs: object, target: Path) -> dict[str, str]:
        if not isinstance(outputs, dict) or not cls.REPORTS.issubset(outputs):
            raise RuntimeError(f"matrix checkpoint evidence is incomplete: {target}")
        if any(not isinstance(outputs[key], str) or not outputs[key].strip() for key in cls.REPORTS):
            raise RuntimeError(f"matrix checkpoint evidence is empty: {target}")
        return {str(key): str(value) for key, value in outputs.items()}

    @staticmethod
    def _read(target: Path) -> object:
        try:
            return json.loads(target.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise RuntimeError(f"invalid matrix journal {target}: {error}") from error

    @staticmethod
    def _write(target: Path, payload: object) -> None:
        target.parent.mkdir(parents=True, exist_ok=True)
        temporary = target.with_suffix(".tmp")
        temporary.write_text(json.dumps(payload, ensure_ascii=False, sort_keys=True) + "\n", encoding="utf-8")
        temporary.replace(target)
