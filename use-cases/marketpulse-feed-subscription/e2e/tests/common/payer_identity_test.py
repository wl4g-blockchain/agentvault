from __future__ import annotations

import unittest

from common.payer_identity import LANGUAGES, PAYER_IDENTITIES, payer_identity


class PayerIdentityTest(unittest.TestCase):
    def test_each_language_owns_an_independent_business_and_wallet_boundary(self) -> None:
        self.assertEqual(set(PAYER_IDENTITIES), set(LANGUAGES))
        identities = list(PAYER_IDENTITIES.values())
        for attribute in (
            "client_id", "scenario_id", "policy_id", "approver_principal",
            "primary_wallet_id", "secondary_wallet_id", "mqtt_username", "approver_mqtt_username",
        ):
            values = [getattr(identity, attribute) for identity in identities]
            self.assertEqual(len(values), len(set(values)), attribute)
        wallets = [wallet for identity in identities for wallet in identity.wallet_ids]
        self.assertEqual(len(wallets), 10)
        self.assertEqual(len(set(wallets)), 10)

    def test_unknown_language_fails_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "unsupported payer language"):
            payer_identity("unknown")


if __name__ == "__main__":
    unittest.main()
