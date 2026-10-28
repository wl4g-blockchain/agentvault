import base64
import copy
import json
import unittest
from dataclasses import replace

from mpp import Challenge, Credential
from mpp.extensions.mcp.constants import META_CREDENTIAL

from agentvault.model import AssetIdentity, HttpCredential, PaymentChallenge, PaymentCredential
from agentvault.protocol import MppAdapter


class MppAdapterTest(unittest.TestCase):
    # Public native challenge captured from Rust mpp 0.15.1. No payment signature.
    _CHALLENGE = {
        "id": "1him3PQmFieK-4wyOlTCcOkgMvjpEM29eBTzEo4dGnk",
        "realm": "agentvault-manual",
        "method": "tempo",
        "intent": "charge",
        "request": {
            "_mppx_scope": {"resource": "https://merchant.example/mcp"},
            "amount": "1000000",
            "currency": "0x20c0000000000000000000000000000000000000",
            "externalId": "price-rust-tempo-mcp",
            "methodDetails": {"chainId": 1337},
            "recipient": "0x052b91ad9732d1bce0ddae15a4545e5c65d02443",
        },
        "expires": "2026-10-26T16:30:26Z",
        "opaque": "eyJjaGFsbGVuZ2VfaWQiOiJkMGNmNTQ1Ny1lM2FlLTRkMjEtYjJkMi04ZmQ2NzgxOWE3OGMiLCJwcmljZV9pZCI6InByaWNlLXJ1c3QtdGVtcG8tbWNwIiwicmVzb3VyY2UiOiJodHRwOi8vbWFya2V0cHVsc2UtcnVzdC10ZW1wby5lMmUtYWdlbnR2YXVsdC1tb2NrLXBheWVlLnN2Yy5jbHVzdGVyLmxvY2FsOjgwODEvbWNwIiwic2NlbmFyaW9faWQiOiJzY2VuYXJpby1tYXJrZXRwdWxzZS1mZWVkLXRlbXBvIn0",
        "header": "Payment-Authorization",
    }

    @classmethod
    def _http_credential(cls, challenge: dict) -> tuple[PaymentCredential, Credential]:
        native = Challenge(
            challenge["id"], challenge["method"], challenge["intent"], challenge["request"],
            realm=challenge["realm"], expires=challenge.get("expires"),
            digest=challenge.get("digest"), header=challenge.get("header"),
        )
        native = Challenge.from_www_authenticate(native.to_www_authenticate(native.realm))
        core = Credential(
            replace(native.to_echo(), opaque=challenge.get("opaque"), header=challenge.get("header")),
            {"type": "transaction", "signature": "unsigned-test-fixture"},
            "did:pkh:eip155:1337:0x" + "11" * 20,
        )
        credential = PaymentCredential(
            "mpp", "challenge", "credential", {},
            HttpCredential("https://merchant.example", challenge.get("header", "Authorization"),
                           core.to_authorization()),
        )
        return credential, core

    def test_native_http_and_mcp_credentials_round_trip_without_changing_binding(self) -> None:
        adapter = MppAdapter()
        credential, core = self._http_credential(self._CHALLENGE)
        projected = adapter.mcp_credential(credential)
        self.assertEqual(projected.key, META_CREDENTIAL)
        self.assertEqual(projected.value["challenge"], self._CHALLENGE)
        self.assertIsInstance(projected.value["challenge"]["request"], dict)
        self.assertEqual(projected.value["payload"], core.payload)
        self.assertEqual(projected.value["source"], core.source)
        presented = adapter.presented_mcp_credential({projected.key: projected.value}, 8192)
        recovered = Credential.from_authorization(presented["authorization"])
        self.assertEqual(recovered, core)
        self.assertEqual(recovered.challenge.request, core.challenge.request)
        self.assertEqual(recovered.challenge.opaque, self._CHALLENGE["opaque"])
        self.assertEqual(recovered.challenge.header, self._CHALLENGE["header"])

    def test_raw_opaque_json_bytes_and_header_spelling_survive_mcp_and_a2a(self) -> None:
        challenge = copy.deepcopy(self._CHALLENGE)
        # Deliberately different from canonical JSON: an opaque echo is a bound byte string.
        opaque_json = b'{ "z": "last", "a" : "first" }'
        challenge["opaque"] = base64.urlsafe_b64encode(opaque_json).rstrip(b"=").decode()
        challenge["header"] = "payment-authorization"
        challenge["digest"] = "sha-256=public-fixture"
        adapter = MppAdapter()
        credential, core = self._http_credential(challenge)
        projected = adapter.mcp_credential(credential)
        self.assertEqual(projected.value["challenge"], challenge)
        incoming = adapter.presented_mcp_credential({projected.key: projected.value}, 8192)
        self.assertEqual(Credential.from_authorization(incoming["authorization"]), core)
        message = {
            "jsonrpc": "2.0", "id": "run", "method": "message/send",
            "params": {"message": {
                "messageId": "request", "kind": "message", "role": "user",
                "parts": [{"kind": "text", "text": "news"}],
            }},
        }
        adapter.a2a_apply_credential(message, credential)
        self.assertEqual(message["_meta"][META_CREDENTIAL]["challenge"], challenge)
        incoming = adapter.a2a_presented_credential(message, 8192)
        self.assertEqual(Credential.from_authorization(incoming["authorization"]), core)

    def test_mcp_native_opaque_header_is_not_double_encoded(self) -> None:
        adapter = MppAdapter()
        message = {"jsonrpc": "2.0", "id": "run", "error": {
            "code": -32042,
            "data": {"httpStatus": 402, "challenges": [self._CHALLENGE]},
        }}
        response = adapter.mcp_challenge(message, 8192)
        header = response.headers["www-authenticate"]
        self.assertIn(f'opaque="{self._CHALLENGE["opaque"]}"', header)
        native = Challenge.from_www_authenticate(header)
        self.assertEqual(native.request, self._CHALLENGE["request"])
        self.assertEqual(native.header, "Payment-Authorization")
        raw = self._CHALLENGE["opaque"]
        self.assertEqual(native.opaque, json.loads(base64.urlsafe_b64decode(raw + "=" * (-len(raw) % 4))))
        response = adapter.challenge(PaymentChallenge(
            "scenario", None, "https://merchant.example", "mpp",
            AssetIdentity("eip155:1337", "USDC", 6), "1", "recipient", "nonce", "hash", 2,
            {"www_authenticate": header},
        ))
        self.assertEqual(response.headers["WWW-Authenticate"], header)

    def test_mcp_accepts_absent_opaque_and_rejects_wrong_wire_types(self) -> None:
        adapter = MppAdapter()
        challenge = copy.deepcopy(self._CHALLENGE)
        challenge.pop("opaque")
        challenge.pop("header")
        credential, core = self._http_credential(challenge)
        projected = adapter.mcp_credential(credential)
        presented = adapter.presented_mcp_credential({projected.key: projected.value}, 8192)
        self.assertEqual(Credential.from_authorization(presented["authorization"]), core)
        for field, invalid in (
            ("request", core.challenge.request),
            ("opaque", {"challenge_id": "wrong-object-type"}),
            ("opaque", "not:base64url"),
            ("opaque", "W10"),
            ("opaque", "eyJhIjoxfQ"),
            ("header", "Authorization"),
        ):
            with self.subTest(field=field, invalid=invalid), self.assertRaises(ValueError):
                mutated = copy.deepcopy(projected.value)
                mutated["challenge"][field] = invalid
                adapter.presented_mcp_credential({META_CREDENTIAL: mutated}, 8192)

    def test_noncanonical_http_request_is_rejected_instead_of_changing_its_binding(self) -> None:
        adapter = MppAdapter()
        credential, core = self._http_credential(self._CHALLENGE)
        raw = json.dumps(self._CHALLENGE["request"], indent=2).encode()
        request = base64.urlsafe_b64encode(raw).rstrip(b"=").decode()
        altered = replace(core, challenge=replace(core.challenge, request=request))
        credential = replace(credential, http=replace(credential.http, header_value=altered.to_authorization()))
        with self.assertRaisesRegex(ValueError, "not canonical"):
            adapter.mcp_credential(credential)

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
