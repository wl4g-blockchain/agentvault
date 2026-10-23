import unittest

from mpp import Challenge, Credential

from agentvault.model import AssetIdentity, HttpCredential, PaymentChallenge, PaymentCredential
from agentvault.protocol import MppAdapter


class MppAdapterTest(unittest.TestCase):
    def test_preserves_bearer_and_uses_native_challenge_header(self) -> None:
        adapter = MppAdapter()
        challenge = Challenge(
            "challenge-0123456789abcdef",
            "tempo",
            "charge",
            {"amount": "1000", "currency": "USDC"},
            realm="merchant.example",
            header="Payment-Authorization",
        )
        response = adapter.challenge(PaymentChallenge(
            "scenario", None, "https://merchant.example", "mpp",
            AssetIdentity("eip155:42431", "USDC", 6, "0x" + "11" * 20),
            "1", "recipient", "nonce", "hash", 2,
            {"www_authenticate": challenge.to_www_authenticate(challenge.realm)},
        ))
        self.assertIn("WWW-Authenticate", response.headers)
        self.assertNotIn("Payment-Required", response.headers)
        authorization = Credential(
            challenge.to_echo(), {"type": "hash", "hash": "0x1111"}
        ).to_authorization()
        credential = adapter.presented_credential({
            "authorization": "Bearer application-token",
            "payment-authorization": authorization,
        }, 1024)
        self.assertEqual(credential, {"authorization": authorization})
        with self.assertRaisesRegex(ValueError, "challenged header"):
            adapter.presented_credential({"authorization": authorization}, 1024)
        with self.assertRaisesRegex(ValueError, "challenged header"):
            adapter.credential(PaymentCredential(
                "mpp", "challenge", "credential", {},
                HttpCredential(
                    "https://merchant.example", "Authorization", authorization,
                ),
            ))

    def test_rejects_unimplemented_method_and_intent(self) -> None:
        adapter = MppAdapter()
        for method, intent in (("stripe", "charge"), ("tempo", "session")):
            with self.subTest(method=method, intent=intent):
                challenge = Challenge(
                    "challenge-0123456789abcdef",
                    method,
                    intent,
                    {"amount": "1000", "currency": "USDC"},
                    realm="merchant.example",
                )
                with self.assertRaisesRegex(ValueError, "only.*Tempo charge"):
                    adapter.challenge(PaymentChallenge(
                        "scenario", None, "https://merchant.example", "mpp",
                        AssetIdentity("eip155:42431", "USDC", 6),
                        "1", "recipient", "nonce", "hash", 2,
                        {"www_authenticate": challenge.to_www_authenticate(challenge.realm)},
                    ))
