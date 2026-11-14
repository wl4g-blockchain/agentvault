import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from verifier.otel.s50_metrics import E2ES50MetricsVerifier as Metrics
from verifier.otel.s51_tracing import E2ES51TracingVerifier as Tracing


class OtelTest(unittest.TestCase):
    def test_metrics_reject_duplicates_and_non_finite_samples(self):
        for text in ("counter_total 1\ncounter_total 2", "counter_total NaN"):
            with self.subTest(text=text), self.assertRaises(RuntimeError):
                Metrics.samples(text)

    def test_trace_parent_must_match_the_logged_producer(self):
        span = {
            "spanID": "consumer",
            "operationName": "http.server.request",
            "references": [{"refType": "CHILD_OF", "spanID": "producer"}],
        }
        Tracing.validate_parents([span], {"producer"})
        with self.assertRaises(RuntimeError):
            Tracing.validate_parents([span], {"forged"})
        with self.assertRaises(RuntimeError):
            Tracing.validate_parents([span], set())

    def test_http_request_log_is_correlated_with_the_jaeger_server_span(self):
        trace_id = "a" * 32
        producer_id = "b" * 16
        payload = {
            "data": [
                {
                    "processes": {"p1": {"serviceName": "agentvault"}},
                    "spans": [
                        {
                            "traceID": trace_id,
                            "spanID": "c" * 16,
                            "operationName": "http.server.request",
                            "duration": 1,
                            "references": [
                                {"refType": "CHILD_OF", "spanID": producer_id}
                            ],
                        }
                    ],
                }
            ]
        }
        evidence = SimpleNamespace(
            payer_language="rust",
            payee_language="go",
            transport="http",
            logs={
                "agentvault": json.dumps(
                    {
                        "event.name": "agentvault.transport.http.request",
                        "trace.id": trace_id,
                        "trace.producer_span_id": producer_id,
                    }
                )
            },
        )
        with tempfile.TemporaryDirectory() as directory:
            runtime = SimpleNamespace(
                context=SimpleNamespace(report_root=Path(directory)),
                _http=Mock(return_value=(200, {}, json.dumps(payload))),
                _url=lambda port, path: path,
            )
            with patch(
                "verifier.otel.s51_tracing.time.monotonic", side_effect=[0, 0, 46]
            ):
                result = json.loads(Tracing.check(runtime, evidence))
            self.assertEqual(result["traces"], {trace_id: 1})
            self.assertTrue(list(Path(directory).rglob(trace_id + ".json")))
