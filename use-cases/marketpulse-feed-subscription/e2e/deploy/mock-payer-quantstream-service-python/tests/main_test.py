from __future__ import annotations

import sys
import unittest
from unittest.mock import patch

from app import main


class MainTest(unittest.TestCase):
    def test_help_exits_before_starting_runtime_dependencies(self) -> None:
        with patch.object(sys, "argv", ["quantstream-python", "--help"]):
            with self.assertRaises(SystemExit) as stopped:
                main.main()
        self.assertEqual(stopped.exception.code, 0)


if __name__ == "__main__":
    unittest.main()
