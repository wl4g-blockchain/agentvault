import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from agentvault.sdk.config import HttpConfig
from agentvault.sdk.transport import HttpVaultTransport


class HttpTest(unittest.TestCase):
    def test_http_context_and_response_limits(self):
        captured = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                captured.append(
                    (
                        self.path,
                        self.headers.get("Authorization"),
                        self.headers.get("traceparent"),
                    )
                )
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"ok":true}')

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            url = f"http://127.0.0.1:{server.server_port}"
            config = HttpConfig(url, authguard_bearer="edge-secret")
            self.assertNotIn("edge-secret", repr(config))
            with HttpVaultTransport(config) as transport:
                self.assertEqual(transport.request("GET", "/api/v1/test"), {"ok": True})
            self.assertEqual(captured[0][:2], ("/api/v1/test", "Bearer edge-secret"))
            self.assertTrue(captured[0][2].startswith("00-"))
            with HttpVaultTransport(HttpConfig(url, max_response_bytes=4)) as transport:
                with self.assertRaisesRegex(RuntimeError, "exceeds"):
                    transport.request("GET", "/api/v1/test")
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
