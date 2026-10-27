"""Catalog invariants for real manual acceptance; no cluster is invoked."""
from __future__ import annotations
import copy
from pathlib import Path
import sys
import unittest
import yaml
ROOT=Path(__file__).resolve().parents[6]
sys.path.insert(0,str(ROOT / "use-cases/marketpulse-feed-subscription/e2e"))
from common.payer_identity import LANGUAGES, payer_identity
from deploy.deployer.manual_profiles import ManualProfiles

class ManualProfilesTest(unittest.TestCase):
    def fixture(self):
        return {"network":"solana:4uhcVJyU9pJkvQyS88uRDiswHXSCkY3z", "mint":"mint-public", "recipient":"recipient-public",
                "tempo_recipient":"0x0000000000000000000000000000000000000111",
                "tempo_key_envs":{f"wallet-quantstream-{language}-{slot}":f"HELPER_SECRET_MANUAL_TEMPO_{language.upper()}_{slot.upper()}"
                                 for language in LANGUAGES for slot in ("primary","secondary")},
                "tempo_wallet_addresses":{f"wallet-quantstream-{language}-{slot}-tempo":f"0x{100+2*index+offset:040x}"
                                          for index,language in enumerate(LANGUAGES) for offset,slot in enumerate(("primary","secondary"))},
                "key_envs":{f"wallet-quantstream-{language}-{slot}":f"HELPER_SECRET_MANUAL_SOLANA_{language.upper()}_{slot.upper()}"
                            for language in LANGUAGES for slot in ("primary","secondary")}}

    def test_three_profiles_reference_only_their_own_wallets_policies_and_prices(self):
        fixture=self.fixture(); untouched=copy.deepcopy(fixture)
        docs=list(yaml.safe_load_all(ManualProfiles.resources(fixture)))
        self.assertEqual(fixture,untouched)
        ids=[(d["kind"],d["metadata"]["name"]) for d in docs]
        self.assertEqual(len(ids),len(set(ids)))
        by_kind={kind:{d["metadata"]["name"]:d["data"] for d in docs if d["kind"]==kind} for kind in ("Wallet","Policy","Scenario","Price","Client")}
        self.assertEqual(len(by_kind["Client"]),6)
        self.assertEqual(len(by_kind["Price"]),30)
        for profile in ManualProfiles.NAMES:
            suffix=ManualProfiles.suffix(profile)
            asset=ManualProfiles.asset(profile,fixture)
            for language in LANGUAGES:
                identity=payer_identity(language)
                policy=by_kind["Policy"][identity.policy_id+suffix]
                self.assertEqual(policy["scenario_id"],identity.scenario_id+suffix)
                self.assertEqual(policy["assets"],[asset])
                self.assertEqual(policy["protocols"],["mpp" if profile=="tempo" else "x402"])
                for wallet_id in policy["wallet_ids"]:
                    self.assertIn(wallet_id,by_kind["Wallet"])
                    self.assertTrue(wallet_id.endswith(suffix) if suffix else not wallet_id.endswith(("-tempo","-solana")))
                for message in ("mcp","a2a"):
                    price=by_kind["Price"][f"price-{language}-{profile}-{message}"]
                    self.assertEqual(price["resource"],ManualProfiles.origin(language,profile)+"/"+message)
                    self.assertEqual(price["asset"],asset)
                    self.assertIn(price["scenario_id"],by_kind["Scenario"])
                    self.assertIn(price["policy_id"],by_kind["Policy"])
                    self.assertIn(price["wallet_id"],by_kind["Wallet"])

    def test_solana_custody_is_a_secret_reference_and_recipient_is_external(self):
        docs=list(yaml.safe_load_all(ManualProfiles.resources(self.fixture())))
        wallets=[d for d in docs if d["kind"]=="Wallet" and d["metadata"]["name"].endswith("-solana")]
        for wallet in wallets:
            data=wallet["data"]
            self.assertEqual(data["algorithm"],"ed25519")
            if data["custody"]=="external": self.assertEqual(data["address"],"recipient-public")
            else:
                self.assertTrue(data["private_key_env"].startswith("HELPER_SECRET_MANUAL_SOLANA_"))
                self.assertNotIn("private_key",data)

    def test_each_chain_uses_distinct_custody_keys_and_recipient_wallet(self):
        docs=list(yaml.safe_load_all(ManualProfiles.resources(self.fixture())))
        wallets=[d["data"] for d in docs if d["kind"]=="Wallet"]
        key_refs=[w["private_key_env"] for w in wallets if w["custody"]!="external"]
        self.assertEqual(len(key_refs),30)
        self.assertEqual(len(set(key_refs)),30)
        recipients=[w["address"] for w in wallets if w["custody"]=="external"]
        self.assertEqual(len(recipients),3)
        self.assertEqual(len(set(recipients)),3)
        for policy in [d["data"] for d in docs if d["kind"]=="Policy" and d["metadata"]["name"].endswith("-tempo")]:
            self.assertEqual(policy["allowed_recipients"],[self.fixture()["tempo_recipient"]])

    def test_cross_language_payer_environment_has_selected_scope_and_transport(self):
        for payer in LANGUAGES:
            for payee in LANGUAGES:
                env=ManualProfiles.payer_environment(payer,payee,"solana","a2a","mqtt")
                self.assertEqual(env["QUANTSTREAM_CLIENT_ID"],payer_identity(payer).client_id)
                self.assertEqual(env["QUANTSTREAM_SCENARIO_ID"],payer_identity(payer).scenario_id+"-solana")
                self.assertEqual(env["MARKETPULSE_URL"],ManualProfiles.origin(payee,"solana")+"/mcp")
                self.assertEqual(env["MARKETPULSE_A2A_URL"],ManualProfiles.origin(payee,"solana")+"/a2a")
                self.assertEqual(env["MARKETPULSE_MESSAGE_PROTOCOL"],"a2a")
                self.assertEqual(env["AGENTVAULT_TRANSPORT"],"mqtt")
                self.assertFalse(any("PRIVATE_KEY" in key or "PASSWORD" in key for key in env))

    def test_unknown_chain_profile_is_rejected(self):
        with self.assertRaises(ValueError): ManualProfiles.suffix("mainnet")
