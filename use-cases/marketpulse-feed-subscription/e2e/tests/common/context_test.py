from __future__ import annotations

import unittest
from pathlib import Path

from common.context import VerificationContext


class VerificationContextTest(unittest.TestCase):
    def test_selected_matrix_languages_are_forwarded_to_the_auditor(self) -> None:
        context = VerificationContext(
            run_id="run",
            report_root=Path("reports"),
            deploy_root=Path("deploy"),
            scenario="full",
            round_number=1,
            payer_language="rust",
            payee_language="java",
            transport="mqtt",
            deployer="docker",
            timeout_seconds=60,
            skip_build=True,
            reuse_build=False,
        )

        environment = context.environment()

        self.assertEqual(environment["E2E_AUDITOR_PAYER_LANGUAGE"], "rust")
        self.assertEqual(environment["E2E_AUDITOR_PAYEE_LANGUAGE"], "java")
        self.assertEqual(environment["E2E_QUANTSTREAM_CLIENT_ID"], "client-quantstream-rust")
        self.assertEqual(
            environment["E2E_QUANTSTREAM_SCENARIO_ID"],
            "scenario-quantstream-rust-market-ingestion",
        )
        self.assertEqual(environment["E2E_QUANTSTREAM_MQTT_USERNAME"], "quantstream-rust-workload")
        self.assertEqual(
            environment["E2E_QUANTSTREAM_UI_MQTT_USERNAME"],
            "quantstream-rust-approver",
        )


if __name__ == "__main__":
    unittest.main()
