from __future__ import annotations

import unittest

from runner import parser, selected_scenarios


class RunnerTest(unittest.TestCase):
    def test_sdk_scope_defaults_to_simple(self) -> None:
        self.assertEqual(parser().parse_args(["-s", "matrix"]).e2e_sdk_scope, "simple")

    def test_sdk_scope_accepts_full(self) -> None:
        self.assertEqual(
            parser().parse_args(["-s", "matrix", "--e2e-sdk-scope", "full"]).e2e_sdk_scope,
            "full",
        )

    def test_advertises_explicit_mpp_tempo_gate(self) -> None:
        self.assertEqual(selected_scenarios(["mpp-tempo"])[0].report_name, "s25_mpp_tempo.md")

    def test_advertises_explicit_x402_solana_gate(self) -> None:
        self.assertEqual(selected_scenarios(["x402-solana"])[0].report_name, "s26_x402_solana.md")


if __name__ == "__main__":
    unittest.main()
