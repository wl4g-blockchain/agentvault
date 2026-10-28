from __future__ import annotations
import copy
import json
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
ROOT=Path(__file__).resolve().parents[6]
sys.path.insert(0,str(ROOT / "use-cases/marketpulse-feed-subscription/e2e"))

from common.payer_identity import LANGUAGES
from deploy.deployer.manual_fixture import ManualChainFixture

class SolanaFixturePort:
    def __init__(self): self.commands=[]; self.calls=[]
    def _solana_command(self,*args,**kwargs): self.commands.append(args); return ""
    def _solana_pubkey(self,path): return "pub-"+Path(path).stem
    def _solana_json(self,*args,**kwargs):
        self.calls.append(args)
        if args[1]=="create-token": return {"commandOutput":{"address":self._solana_pubkey(args[-1])}}
        if args[1]=="address": return {"associatedTokenAddress":"ata-"+args[-1]}
        return {}
    def _solana_keypair_json(self,path): return json.dumps([37]*64)
    def _kubectl(self,*args): return "\n".join(f"0x{700+index:040x}" for index in range(11))
    @contextmanager
    def _forward_service(self,*args): yield 12345
    def _json_rpc_result(self,*args): return "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3z1234"

class ManualFixtureTest(unittest.TestCase):
    def test_standard_spl_mint_creates_all_ten_wallet_accounts_without_custody_leaking(self):
        port=SolanaFixturePort()
        public,secrets=ManualChainFixture(port).solana()
        self.assertEqual(public["token_program"],"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
        self.assertEqual(public["decimals"],6)
        self.assertEqual(len(public["accounts"]),11)
        self.assertEqual(len(public["key_envs"]),10)
        self.assertEqual(len(secrets),21)
        self.assertEqual(public["network"],"solana:4uhcVJyU9pJkvQyS88uRDiswHXSCkY3z")
        minted=[call for call in port.calls if call[1]=="mint"]
        self.assertEqual(len(minted),10)
        self.assertTrue(all(call[-2]=="1000" for call in minted))
        for call in port.calls:
            if call[1] in {"create-token","create-account","address","mint"}:
                self.assertIn("--fee-payer",call)
        wire=json.dumps(public)
        for secret in secrets.values(): self.assertNotIn(secret,wire)
        for language in LANGUAGES:
            self.assertIn(f"wallet-quantstream-{language}-primary",public["key_envs"])

    def test_unexpected_created_mint_aborts_before_any_wallet_is_funded(self):
        port=SolanaFixturePort(); port._solana_json=Mock(return_value={"commandOutput":{"address":"another-mint"}})
        with self.assertRaisesRegex(RuntimeError,"mint identity mismatch"): ManualChainFixture(port).solana()
        self.assertEqual(port._solana_json.call_count,1)

    def test_invalid_genesis_hash_never_returns_custody_configuration(self):
        port=SolanaFixturePort(); port._json_rpc_result=Mock(return_value="not a chain!")
        with self.assertRaisesRegex(RuntimeError,"genesis identity"): ManualChainFixture(port).solana()

    def test_tempo_keys_are_distinct_and_funding_contains_only_public_addresses(self):
        port=SolanaFixturePort()
        fixture,secrets=ManualChainFixture(port).tempo_wallets()
        self.assertEqual(len(fixture["tempo_wallet_addresses"]),10)
        self.assertEqual(len(set(secrets.values())),10)
        self.assertNotIn(fixture["tempo_recipient"],fixture["tempo_wallet_addresses"].values())
        bodies=[]
        def http(url,method,body,headers,timeout):
            bodies.append(json.loads(body))
            return 200,{},json.dumps({"transaction":"0xfunded"})
        port._http=http
        transactions=ManualChainFixture(port).fund_tempo(fixture)
        self.assertEqual(set(transactions),set(fixture["tempo_wallet_addresses"]))
        self.assertEqual(len(bodies),10)
        self.assertTrue(all(set(body)=={"address"} for body in bodies))
        for body in bodies: self.assertIn(body["address"],fixture["tempo_wallet_addresses"].values())

    def test_duplicate_tempo_addresses_fail_before_funding(self):
        port=SolanaFixturePort()
        port._kubectl=Mock(return_value="\n".join(["0x"+"1"*40]*11))
        with self.assertRaisesRegex(RuntimeError,"distinct EVM addresses"): ManualChainFixture(port).tempo_wallets()
