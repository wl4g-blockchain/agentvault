from __future__ import annotations

import asyncio
import unittest

from app.handler.ingestion import IngestionHandler


class IngestionHandlerTest(unittest.TestCase):
    def test_health_identifies_the_python_ingestor(self) -> None:
        handler = IngestionHandler()
        body = asyncio.run(handler.health())
        self.assertEqual(body, {"status": "ready", "service": "quantstream-python"})
        metrics = asyncio.run(handler.metrics())
        self.assertIn(b"quantstream_up 1", metrics.body)


if __name__ == "__main__":
    unittest.main()
