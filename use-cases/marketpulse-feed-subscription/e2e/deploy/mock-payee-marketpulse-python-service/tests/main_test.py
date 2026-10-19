import unittest

from app.main import main


class MainTest(unittest.TestCase):
    def test_main_is_a_thin_server_entrypoint(self) -> None:
        self.assertTrue(callable(main))


if __name__ == "__main__":
    unittest.main()
