from __future__ import annotations

import unittest

from common.sdk_matrix import matrix_cases, matrix_languages


class SdkMatrixTest(unittest.TestCase):
    def test_simple_scope_is_the_default_fast_rust_and_go_cross_product(self) -> None:
        cases = matrix_cases("simple")

        self.assertEqual(matrix_languages("simple"), ("rust", "go"))
        self.assertEqual(len(cases), 8)
        self.assertEqual(
            {case.name for case in cases},
            {
                f"payer={payer} payee={payee} transport={transport}"
                for transport in ("mqtt", "grpc")
                for payer in ("rust", "go")
                for payee in ("rust", "go")
            },
        )

    def test_full_scope_is_the_five_language_cross_product(self) -> None:
        self.assertEqual(len(matrix_languages("full")), 5)
        self.assertEqual(len(matrix_cases("full")), 50)

    def test_unknown_scope_fails_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "unsupported E2E SDK scope"):
            matrix_cases("unknown")


if __name__ == "__main__":
    unittest.main()
