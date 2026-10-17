import unittest

from agentvault.transport.http import HttpVaultTransport


class HttpTransportTest(unittest.TestCase):
    def test_rejects_unsafe_authority(self):
        with self.assertRaises(ValueError):
            HttpVaultTransport("https://user@vault.example")
