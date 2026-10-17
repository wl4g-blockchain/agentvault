import unittest

from agentvault.model import ApprovalDecisionKind, ApprovalStatus


class ApprovalModelTest(unittest.TestCase):
    def test_approval_wire_values_are_stable_snake_case(self):
        self.assertEqual(ApprovalStatus.PENDING.value, "pending")
        self.assertEqual(ApprovalStatus.CONSUMED.value, "consumed")
        self.assertEqual(ApprovalDecisionKind.APPROVE.value, "approve")
