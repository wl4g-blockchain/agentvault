import tempfile
import unittest
from pathlib import Path

from app.storage.feed import FeedSqliteRepository


class FeedSqliteRepositoryTest(unittest.TestCase):
    def test_initializes_exactly_one_hundred_rows(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            storage = FeedSqliteRepository(str(Path(directory) / "marketpulse.sqlite3"))
            self.assertEqual(50, len(storage.news("yesterday", 100)))
            self.assertEqual(50, len(storage.news("realtime", 100)))
            storage.close()


if __name__ == "__main__":
    unittest.main()
