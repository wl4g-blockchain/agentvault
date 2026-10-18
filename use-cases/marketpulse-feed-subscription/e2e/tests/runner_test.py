from __future__ import annotations

import unittest

from runner import parser


class RunnerTest(unittest.TestCase):
    def test_sdk_scope_defaults_to_simple(self) -> None:
        self.assertEqual(parser().parse_args(["-s", "matrix"]).e2e_sdk_scope, "simple")

    def test_sdk_scope_accepts_full(self) -> None:
        self.assertEqual(
            parser().parse_args(["-s", "matrix", "--e2e-sdk-scope", "full"]).e2e_sdk_scope,
            "full",
        )


if __name__ == "__main__":
    unittest.main()
