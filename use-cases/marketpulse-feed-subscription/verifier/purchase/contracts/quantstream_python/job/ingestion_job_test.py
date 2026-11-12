from __future__ import annotations

import sqlite3
import tempfile
import unittest
from pathlib import Path

from app.agent.mcp import ToolResult
from app.job.ingestion_job import _persist


class IngestionJobTest(unittest.TestCase):
    def test_completed_free_and_paid_results_are_persisted(self) -> None:
        with tempfile.TemporaryDirectory(prefix="agentvault-python-") as directory:
            database_path = Path(directory) / "ingestion.sqlite3"
            _persist(str(database_path), "run-python-1", ToolResult(b'{"paid":false}'),
                     ToolResult(b'{"paid":true}', "intent-python-1"))
            with sqlite3.connect(database_path) as database:
                row = database.execute(
                    "SELECT run_id,payment_intent_id FROM ingestion_run"
                ).fetchone()
        self.assertEqual(row, ("run-python-1", "intent-python-1"))


if __name__ == "__main__":
    unittest.main()
