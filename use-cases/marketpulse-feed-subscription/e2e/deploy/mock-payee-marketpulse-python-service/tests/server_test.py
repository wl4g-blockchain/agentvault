import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from fastapi.testclient import TestClient

from app.server import MarketPulseServer, _parse_bind


class MarketPulseServerTest(unittest.TestCase):
    def test_owns_mcp_and_operational_routes(self) -> None:
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
            "MARKETPULSE_BIND": "127.0.0.1:18081",
            "AGENTVAULT_TRANSPORT": "mqtt",
            "AGENTVAULT_MQTT_HOST": "127.0.0.1",
            "AGENTVAULT_MQTT_PORT": "1883",
            "AGENTVAULT_MQTT_TLS": "false",
            "AGENTVAULT_MQTT_USERNAME": "marketpulse-payee",
            "AGENTVAULT_MQTT_PASSWORD": "test-only",
            "AGENTVAULT_MQTT_CLIENT_ID": "client-marketpulse",
            "AGENTVAULT_MQTT_TIMEOUT_SECONDS": "1",
            "MARKETPULSE_DATABASE": str(Path(directory) / "marketpulse.sqlite3"),
            "MARKETPULSE_SCENARIO_ID": "scenario-test",
            "MARKETPULSE_PRICE_ID": "price-test",
            "MARKETPULSE_RESOURCE_URL": "http://127.0.0.1:18081/mcp",
            "MARKETPULSE_ORIGIN": "http://127.0.0.1:18081",
        }, clear=False):
            server = MarketPulseServer.from_environment()
            try:
                client = TestClient(server.application)
                self.assertEqual(200, client.get("/healthz").status_code)
                self.assertIn("marketpulse_up 1", client.get("/metrics").text)
                self.assertNotEqual(404, client.post("/mcp", content=b"{}").status_code)
            finally:
                server.close()

    def test_validates_listener(self) -> None:
        self.assertEqual(("127.0.0.1", 8081), _parse_bind("127.0.0.1:8081"))
        with self.assertRaises(RuntimeError):
            _parse_bind("invalid")


if __name__ == "__main__":
    unittest.main()
