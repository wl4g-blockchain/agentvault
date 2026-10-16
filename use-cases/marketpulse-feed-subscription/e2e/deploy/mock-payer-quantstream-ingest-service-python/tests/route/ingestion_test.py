from __future__ import annotations

import unittest

from fastapi import FastAPI
from fastapi.testclient import TestClient

from app.route.ingestion import IngestionRouter


class IngestionRouterTest(unittest.TestCase):
    def test_router_registers_only_operational_endpoints(self) -> None:
        app = FastAPI()
        IngestionRouter().register(app)
        with TestClient(app) as client:
            self.assertEqual(client.get("/healthz").status_code, 200)
            self.assertEqual(client.get("/metrics").status_code, 200)
            self.assertEqual(client.post("/api/v1/ingestion").status_code, 404)
            self.assertEqual(client.get("/undeclared").status_code, 404)


if __name__ == "__main__":
    unittest.main()
