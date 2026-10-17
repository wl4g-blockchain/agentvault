import unittest

from app.handler.feed import FeedHandler


class FeedHandlerTest(unittest.TestCase):
    def test_declares_operational_and_mcp_operations(self) -> None:
        self.assertTrue(callable(FeedHandler.health))
        self.assertTrue(callable(FeedHandler.metrics))
        self.assertTrue(callable(FeedHandler.mcp))


if __name__ == "__main__":
    unittest.main()
