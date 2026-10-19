import unittest

from agentvault.config import GrpcConfig


class GrpcTransportTest(unittest.TestCase):
    def test_rejects_remote_cleartext(self):
        with self.assertRaises(ValueError):
            GrpcConfig("vault.example:50051", tls=False)
