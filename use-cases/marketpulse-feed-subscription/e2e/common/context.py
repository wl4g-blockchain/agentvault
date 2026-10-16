from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class VerificationContext:
    run_id: str
    report_root: Path
    deploy_root: Path
    scenario: str
    round_number: int
    payer_language: str
    payee_language: str
    deployer: str
    timeout_seconds: float
    skip_build: bool
    reuse_build: bool

    def environment(self) -> dict[str, str]:
        return {
            "E2E_RUNNER_RUN_ID": self.run_id,
            "E2E_RUNNER_DEPLOYER": self.deployer,
            "E2E_RUNNER_SKIP_BUILD": "1" if self.skip_build else "0",
            "E2E_RUNNER_REUSE_BUILD": "1" if self.reuse_build else "0",
            "E2E_RUNNER_PAYER_LANGUAGE": self.payer_language,
            "E2E_RUNNER_PAYEE_LANGUAGE": self.payee_language,
            # The runner is the lifecycle owner. It retains the stack unless
            # --cleanup-after-run is explicitly requested.
            "E2E_RUNNER_KEEP_STACK": "1",
        }
