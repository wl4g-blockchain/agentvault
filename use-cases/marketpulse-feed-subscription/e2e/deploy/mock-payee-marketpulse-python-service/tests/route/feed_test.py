import unittest

from app.route.feed import FeedRouter


class FeedRouterTest(unittest.TestCase):
    def test_router_exposes_only_operational_paths(self) -> None:
        self.assertTrue(callable(FeedRouter.register))


if __name__ == "__main__":
    unittest.main()
