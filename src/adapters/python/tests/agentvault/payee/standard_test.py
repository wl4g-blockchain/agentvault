import unittest

from agentvault.payee.standard import StandardPayeeMiddleware


class StandardPayeeMiddlewareTest(unittest.TestCase):
    def test_wraps_only_the_core(self):
        core = object()
        self.assertIs(StandardPayeeMiddleware(core).core, core)
