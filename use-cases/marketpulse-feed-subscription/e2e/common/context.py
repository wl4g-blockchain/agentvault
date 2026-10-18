from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from common.payer_identity import payer_identity


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

    def environment(self) -> dict[str, str]:
        payer = payer_identity(self.payer_language)
        return {
            "E2E_RUNNER_RUN_ID": self.run_id,
            "E2E_RUNNER_DEPLOYER": self.deployer,
            "E2E_RUNNER_SKIP_BUILD": "1" if self.skip_build else "0",
            "E2E_RUNNER_REUSE_BUILD": "1" if self.reuse_build else "0",
            "E2E_MATRIX_WORKERS": str(self.matrix_workers),
            "E2E_SDK_SCOPE": self.e2e_sdk_scope,
            "E2E_WORKER_SLOT": "" if self.worker_slot is None else str(self.worker_slot),
            "E2E_RUNNER_PAYER_LANGUAGE": self.payer_language,
            "E2E_RUNNER_PAYEE_LANGUAGE": self.payee_language,
            "E2E_AGENTVAULT_TRANSPORT": self.transport,
            "E2E_AUDITOR_PAYER_LANGUAGE": self.payer_language,
            "E2E_AUDITOR_PAYEE_LANGUAGE": self.payee_language,
            "E2E_QUANTSTREAM_CLIENT_ID": payer.client_id,
            "E2E_QUANTSTREAM_SCENARIO_ID": payer.scenario_id,
            "E2E_QUANTSTREAM_UI_APPROVER_PRINCIPAL": payer.approver_principal,
            "E2E_QUANTSTREAM_MQTT_USERNAME": payer.mqtt_username,
            "E2E_QUANTSTREAM_UI_MQTT_USERNAME": payer.approver_mqtt_username,
            # The runner is the lifecycle owner. It retains the stack unless
            # --cleanup-after-run is explicitly requested.
            "E2E_RUNNER_KEEP_STACK": "1",
        }
