import unittest

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
            "operationName": "mqtt.server.request",
            "references": [{"refType": "CHILD_OF", "spanID": "producer"}],
        }
        Tracing.validate_parents([span], {"producer"}, "mqtt")
        with self.assertRaises(RuntimeError):
            Tracing.validate_parents([span], {"forged"}, "mqtt")
        with self.assertRaises(RuntimeError):
            Tracing.validate_parents([span], set(), "mqtt")
