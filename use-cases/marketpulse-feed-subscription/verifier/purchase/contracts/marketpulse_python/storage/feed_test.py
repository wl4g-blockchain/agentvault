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

class A2ATaskPersistenceTest(unittest.IsolatedAsyncioTestCase):
    async def test_application_task_scope_survives_reopen(self):
        from a2a.types import Task, TaskStatus
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "marketpulse.sqlite3")
            repository = FeedSqliteRepository(path)
            await repository.save(Task(id="task-1", context_id="context-1", status=TaskStatus(state="submitted"),
                                       metadata={"agentvault.payment.run_id":"run-1"}))
            repository.close()
            repository = FeedSqliteRepository(path)
            try:
                task = await repository.get("task-1")
                self.assertEqual(task.context_id, "context-1")
                self.assertEqual(task.metadata["agentvault.payment.run_id"], "run-1")
            finally:
                repository.close()


if __name__ == "__main__":
    unittest.main()
