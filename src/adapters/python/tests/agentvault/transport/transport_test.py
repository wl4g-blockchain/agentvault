import unittest

from agentvault.transport.transport import APIError


class TransportTest(unittest.TestCase):
    def test_api_error_preserves_status_and_code(self) -> None:
        error = APIError(409, "RUN_CONFLICT", "conflict")
        self.assertEqual((409, "RUN_CONFLICT"), (error.status, error.code))
        self.assertNotIn("secret", repr(error))
