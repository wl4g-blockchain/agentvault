import unittest

from agentvault.model import OperationContext, PayerRequestContext
from agentvault.model.payer import ProtocolEvidence, ReportOutboundResponseRequest


class PayerModelTest(unittest.TestCase):
    def test_payer_context_is_stable(self):
        first = PayerRequestContext("run-1")
        retry = PayerRequestContext("run-1")
        self.assertEqual(first, retry)
        self.assertEqual(first.run_id, "run-1")

    def test_receipt_report_requires_protocol_evidence(self):
        context = OperationContext("scenario", "run")
        evidence = ProtocolEvidence("payment-receipt", "receipt")
        request = ReportOutboundResponseRequest(context, "mpp", evidence)
        self.assertEqual(request.wire(), {
            "scenario_id": "scenario", "run_id": "run", "protocol": "mpp",
            "evidence": {"header_name": "payment-receipt", "header_value": "receipt"},
        })
        with self.assertRaises(TypeError):
            ReportOutboundResponseRequest(context, payment_response="receipt")
        with self.assertRaises(ValueError):
            ReportOutboundResponseRequest(context, "mpp", None)
        with self.assertRaises(ValueError):
            ProtocolEvidence("événement", "receipt")
